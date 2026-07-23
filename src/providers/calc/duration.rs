use super::util::{relative_secs, result_calc};
use crate::providers::SearchResult;
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) fn try_duration_expr(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase();
    // Must look like multi-unit duration, not plain math or conversion
    if lower.contains(" to ") || lower.contains(" in ") || lower.contains(" as ") {
        return None;
    }
    static RE_TOKEN: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)([+-])?\s*(\d+(?:\.\d+)?)\s*",
            r"(hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s|days?|d|weeks?|w)",
        ))
        .unwrap()
    });
    let mut total_secs: f64 = 0.0;
    let mut count = 0;
    let mut last_end = 0;
    let mut has_op = false;

    for cap in RE_TOKEN.captures_iter(&lower) {
        let m = cap.get(0)?;
        // allow only whitespace between tokens
        let between = lower[last_end..m.start()].trim();
        if last_end > 0 && !between.is_empty() && between != "+" && between != "-" {
            // first char of between might be absorbed in sign group
            if !matches!(between, "+" | "-" | "") {
                // e.g. garbage
                if !between.chars().all(|c| c.is_whitespace()) {
                    return None;
                }
            }
        }
        if between == "+" || between == "-" {
            has_op = true;
        }
        let sign_g = cap.get(1).map(|x| x.as_str()).unwrap_or("");
        let mut sign = 1.0_f64;
        if sign_g == "-" {
            sign = -1.0;
            has_op = true;
        } else if sign_g == "+" {
            has_op = true;
        } else if between == "-" {
            sign = -1.0;
        }
        let n: f64 = cap.get(2)?.as_str().parse().ok()?;
        let unit = cap.get(3)?.as_str();
        let secs = relative_secs(n, unit)?;
        total_secs += sign * secs;
        count += 1;
        last_end = m.end();
    }

    if count < 2 {
        return None;
    }
    // trailing junk?
    if !lower[last_end..].trim().is_empty() {
        return None;
    }
    // Prefer when there's an operator OR multiple units stacked (10h 30min)
    if !has_op && count < 2 {
        return None;
    }

    let formatted = format_duration(total_secs.abs());
    let title = if total_secs < 0.0 {
        format!("-{formatted}")
    } else {
        formatted
    };
    Some(result_calc(title.clone(), format!("duration · {q}"), title))
}

pub(crate) fn format_duration(secs: f64) -> String {
    if secs < 0.001 {
        return "0s".into();
    }
    let mut rem = secs.round() as i64;
    let days = rem / 86400;
    rem %= 86400;
    let hours = rem / 3600;
    rem %= 3600;
    let mins = rem / 60;
    let s = rem % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if mins > 0 {
        parts.push(format!("{mins}min"));
    }
    if s > 0 && days == 0 {
        parts.push(format!("{s}s"));
    }
    if parts.is_empty() {
        "0s".into()
    } else {
        parts.join(" ")
    }
}
