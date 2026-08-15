use super::timezone::parse_clock;
use super::util::{card_result, relative_secs};
use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) fn try_duration_expr(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase();

    // Clock range: "7:26 - 9:32", "7:26am to 9:32pm", "22:00 - 6:30"
    if let Some(r) = try_clock_range(&lower, q) {
        return Some(r);
    }

    // `N% of <multi-token duration>` → scaled duration (`50% of 1h 30min` → 45min).
    // Single-token (`50% of 2h`) stays in unitmath's percentage card.
    static RE_PCT_OF: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*([+-]?\d+(?:\.\d+)?)\s*%\s*of\s+(.+?)\s*$").unwrap()
    });
    if let Some(c) = RE_PCT_OF.captures(&lower) {
        let rest = c.get(2)?.as_str();
        let (secs, count, any_non_m_unit, _end) = parse_duration_tokens(rest)?;
        if count >= 2 && any_non_m_unit {
            let pct: f64 = c.get(1)?.as_str().parse().ok()?;
            let out = secs * pct / 100.0;
            let formatted = format_duration(out.abs());
            let title = if out < 0.0 { format!("-{formatted}") } else { formatted };
            let shown = format!("{}% of {rest}", c.get(1)?.as_str());
            return Some(card_result(
                title.clone(),
                format!("= {shown}"),
                title.clone(),
                shown,
                "percentage",
                title,
                "result",
            ));
        }
    }

    // Must look like multi-unit duration, not plain math or conversion
    if lower.contains(" to ") || lower.contains(" in ") || lower.contains(" as ") {
        return None;
    }
    let (mut total_secs, count, any_non_m_unit, last_end) = parse_duration_tokens(&lower)?;

    // Optional ×/÷ by a dimensionless number (`2min 16 sec * 5`, `1h / 2`).
    let mut scale: Option<f64> = None;
    let mut divide = false;
    let trailing = lower[last_end..].trim();
    if !trailing.is_empty() {
        static RE_SCALE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"^(?:×|\*|/|÷)\s*([+-]?\d+(?:\.\d+)?)\s*$").unwrap());
        let c = RE_SCALE.captures(trailing)?;
        divide = trailing.starts_with('/') || trailing.starts_with('÷');
        scale = Some(c.get(1)?.as_str().parse().ok()?);
    }

    if count < 2 && scale.is_none() {
        return None;
    }
    // All-bare-`m` expressions (`100m + 5m`, `30m + 30m`) are meters/length,
    // not minutes — reject so the unit engine can own them.
    if !any_non_m_unit {
        return None;
    }
    if let Some(s) = scale {
        if s == 0.0 {
            return None;
        }
        total_secs = if divide { total_secs / s } else { total_secs * s };
    }

    let formatted = format_duration(total_secs.abs());
    let title = if total_secs < 0.0 {
        format!("-{formatted}")
    } else {
        formatted
    };
    Some(card_result(
        title.clone(),
        format!("= {q}"),
        title.clone(),
        q.to_string(),
        "duration",
        title,
        "result",
    ))
}

/// Parse a duration expression's time tokens (whitespace or explicit `+`/`-`
/// separators). Returns `(total_secs, token_count, any_non_m_unit, end)`
/// where `end` is the byte offset past the last token. Rejects leading /
/// embedded junk (`in `, `50% of `, `about `) so those queries can't be
/// swallowed into a duration.
fn parse_duration_tokens(s: &str) -> Option<(f64, usize, bool, usize)> {
    static RE_TOKEN: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)([+-])?\s*(\d+(?:\.\d+)?)\s*",
            r"(hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s|days?|d|weeks?|w)",
        ))
        .unwrap()
    });
    let mut total_secs: f64 = 0.0;
    let mut count = 0;
    let mut any_non_m_unit = false;
    let mut last_end = 0;

    for cap in RE_TOKEN.captures_iter(s) {
        let m = cap.get(0)?;
        let between = s[last_end..m.start()].trim();
        if !between.is_empty() && between != "+" && between != "-" {
            return None;
        }
        let sign_g = cap.get(1).map(|x| x.as_str()).unwrap_or("");
        let mut sign = 1.0_f64;
        if sign_g == "-" || between == "-" {
            sign = -1.0;
        }
        let n: f64 = cap.get(2)?.as_str().parse().ok()?;
        let unit = cap.get(3)?.as_str();
        if unit != "m" {
            any_non_m_unit = true;
        }
        let secs = relative_secs(n, unit)?;
        total_secs += sign * secs;
        count += 1;
        last_end = m.end();
    }
    Some((total_secs, count, any_non_m_unit, last_end))
}

/// Difference between two clock times on the same day (or overnight if end < start).
///
/// Accepts: `7:26 - 9:32`, `7:26-9:32`, `7:26 to 9:32`, `7:26am - 9:32pm`,
/// `7:26:15 - 9:32:00`, `9pm - 11:30pm`.
fn try_clock_range(lower: &str, original: &str) -> Option<SearchResult> {
    // TIME SEP TIME — require at least one :mm (or am/pm on both sides) so bare
    // math like "7 - 9" is not stolen.
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*",
            r"(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?",
            r"\s*(?:-|–|—|to|until|till)\s*",
            r"(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?",
            r"\s*$",
        ))
        .unwrap()
    });
    let c = RE.captures(lower)?;

    let start_min = c.get(2);
    let start_ampm = c.get(4);
    let end_min = c.get(6);
    let end_ampm = c.get(8);

    // Need a real clock signal: minutes and/or am/pm on at least one side,
    // and not both sides bare hour-only without am/pm (avoids "7 - 9").
    let start_clocky = start_min.is_some() || start_ampm.is_some();
    let end_clocky = end_min.is_some() || end_ampm.is_some();
    if !start_clocky && !end_clocky {
        return None;
    }
    // If one side is bare hour and the other has only minutes without am/pm,
    // still ok (e.g. "7 - 9:30"). Bare "7pm - 9" is ok via am/pm.

    let (sh, sm, ss) = parse_clock(
        c.get(1)?.as_str(),
        start_min.map(|m| m.as_str()),
        c.get(3).map(|m| m.as_str()),
        start_ampm.map(|m| m.as_str()),
    )?;
    let (eh, em, es) = parse_clock(
        c.get(5)?.as_str(),
        end_min.map(|m| m.as_str()),
        c.get(7).map(|m| m.as_str()),
        end_ampm.map(|m| m.as_str()),
    )?;

    let start_secs = (sh * 3600 + sm * 60 + ss) as i64;
    let mut end_secs = (eh * 3600 + em * 60 + es) as i64;
    // Overnight: end before start → assume next day
    if end_secs < start_secs {
        end_secs += 24 * 3600;
    }
    let delta = (end_secs - start_secs) as f64;
    let formatted = format_duration(delta);
    let display = original.trim().to_string();
    // Raycast-style dual-panel card (same layout as math / unit convert).
    Some(SearchResult {
        id: format!("calc:range:{display}:{formatted}"),
        title: formatted.clone(),
        subtitle: format!("time range · {display}"),
        kind: ResultKind::Calc,
        score: 10_000,
        icon: Some("accessories-calculator".into()),
        action: Action::Copy(formatted.clone()),
        conversion: Some(ConversionView {
            left_title: display,
            left_badge: "time range".into(),
            right_title: formatted,
            right_badge: "duration".into(),
        }),
    })
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

#[cfg(test)]
mod tests {
    use super::{format_duration, try_duration_expr};

    #[test]
    fn clock_range_basic() {
        let r = try_duration_expr("7:26 - 9:32").expect("range");
        assert_eq!(r.title, "2h 6min");
        let conv = r.conversion.expect("card layout");
        assert_eq!(conv.left_title, "7:26 - 9:32");
        assert_eq!(conv.left_badge, "time range");
        assert_eq!(conv.right_title, "2h 6min");
        assert_eq!(conv.right_badge, "duration");
    }

    #[test]
    fn clock_range_no_spaces() {
        let r = try_duration_expr("7:26-9:32").expect("range");
        assert_eq!(r.title, "2h 6min");
    }

    #[test]
    fn clock_range_to_word() {
        let r = try_duration_expr("7:26 to 9:32").expect("range");
        assert_eq!(r.title, "2h 6min");
    }

    #[test]
    fn clock_range_ampm() {
        let r = try_duration_expr("7:26am - 9:32pm").expect("range");
        assert_eq!(r.title, "14h 6min");
    }

    #[test]
    fn clock_range_overnight() {
        let r = try_duration_expr("22:00 - 6:30").expect("range");
        assert_eq!(r.title, "8h 30min");
    }

    #[test]
    fn clock_range_same_time() {
        let r = try_duration_expr("12:00 - 12:00").expect("range");
        assert_eq!(r.title, "0s");
    }

    #[test]
    fn bare_hours_not_stolen() {
        assert!(try_duration_expr("7 - 9").is_none());
    }

    #[test]
    fn unit_duration_still_works() {
        let r = try_duration_expr("10h 30min").expect("units");
        assert_eq!(r.title, "10h 30min");
        let r = try_duration_expr("2h + 30m").expect("ops");
        assert_eq!(r.title, "2h 30min");
    }

    #[test]
    fn scale_by_number() {
        let r = try_duration_expr("2min 16 sec * 5").expect("scale");
        assert_eq!(r.title, "11min 20s");
        let r = try_duration_expr("1h 30min * 2").expect("scale");
        assert_eq!(r.title, "3h");
        let r = try_duration_expr("1.5h * 2").expect("scale");
        assert_eq!(r.title, "3h");
        let r = try_duration_expr("30min / 2").expect("scale");
        assert_eq!(r.title, "15min");
        let r = try_duration_expr("1h / 2").expect("scale");
        assert_eq!(r.title, "30min");
        let r = try_duration_expr("1h ÷ 4").expect("scale");
        assert_eq!(r.title, "15min");
        // Scale still renders on the card.
        let conv = r.conversion.expect("card layout");
        assert_eq!(conv.left_badge, "duration");
        assert_eq!(conv.right_badge, "result");
        // Zero divisor and scale-only-without-time are rejected.
        assert!(try_duration_expr("2h / 0").is_none());
        assert!(try_duration_expr("* 5").is_none());
    }

    #[test]
    fn does_not_steal_leading_junk() {
        assert!(try_duration_expr("in 1h 30min").is_none());
        assert!(try_duration_expr("about 2h 30min").is_none());
        // Single-token `% of` stays in unitmath, not a duration.
        assert!(try_duration_expr("50% of 2h").is_none());
    }

    #[test]
    fn percent_of_multi_token_duration() {
        let r = try_duration_expr("50% of 1h 30min").expect("pct");
        assert_eq!(r.title, "45min");
        let conv = r.conversion.expect("card layout");
        assert_eq!(conv.left_badge, "percentage");
        assert_eq!(conv.right_title, "45min");
        assert_eq!(conv.right_badge, "result");
        let r = try_duration_expr("20% of 2h 30min").expect("pct");
        assert_eq!(r.title, "30min");
        let r = try_duration_expr("10% of 1d 2h").expect("pct");
        assert_eq!(r.title, "2h 36min");
    }

    #[test]
    fn bare_m_is_meters_not_minutes() {
        assert!(try_duration_expr("100m + 5m").is_none());
        assert!(try_duration_expr("30m + 30m").is_none());
        assert!(try_duration_expr("100m").is_none());
        // Minutes alongside an unambiguous time unit still work.
        let r = try_duration_expr("2h + 30m").expect("mixed");
        assert_eq!(r.title, "2h 30min");
        let r = try_duration_expr("1h 30min").expect("stacked");
        assert_eq!(r.title, "1h 30min");
    }

    #[test]
    fn format_duration_parts() {
        assert_eq!(format_duration(0.0), "0s");
        assert_eq!(format_duration(3661.0), "1h 1min 1s");
        assert_eq!(format_duration(2.0 * 3600.0 + 6.0 * 60.0), "2h 6min");
    }
}
