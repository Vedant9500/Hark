use super::util::{relative_secs, result_calc};
use crate::providers::SearchResult;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc};
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) fn try_datetime(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase();
    let now = Local::now();

    if matches!(lower.as_str(), "now" | "time" | "date" | "today") {
        let s = now.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        return Some(result_calc(
            s.clone(),
            format!("Local now · unix {}", now.timestamp()),
            s,
        ));
    }

    if lower == "utc" || lower == "now utc" {
        let s = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
        return Some(result_calc(s.clone(), "UTC now".into(), s));
    }

    if lower == "tomorrow" {
        let d = now + Duration::days(1);
        let s = d.format("%Y-%m-%d (%A)").to_string();
        return Some(result_calc(s.clone(), "Tomorrow".into(), s));
    }

    if lower == "yesterday" {
        let d = now - Duration::days(1);
        let s = d.format("%Y-%m-%d (%A)").to_string();
        return Some(result_calc(s.clone(), "Yesterday".into(), s));
    }

    // unix timestamp
    static RE_UNIX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^\s*(?:unix|epoch|timestamp)\s+([+-]?\d+)\s*$").unwrap());
    if let Some(c) = RE_UNIX.captures(&lower) {
        let ts: i64 = c.get(1)?.as_str().parse().ok()?;
        let dt = chrono::DateTime::from_timestamp(ts, 0)?;
        let local = dt.with_timezone(&Local);
        let s = local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        return Some(result_calc(s.clone(), format!("unix {ts}"), s));
    }

    // bare large epoch
    if let Ok(ts) = lower.parse::<i64>() {
        if (1_000_000_000..4_000_000_000).contains(&ts) {
            let dt = chrono::DateTime::from_timestamp(ts, 0)?;
            let local = dt.with_timezone(&Local);
            let s = local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
            return Some(result_calc(s.clone(), format!("unix {ts}"), s));
        }
    }

    // "to unix" / "unix now"
    if matches!(lower.as_str(), "unix" | "epoch" | "to unix" | "unix now") {
        let ts = now.timestamp().to_string();
        return Some(result_calc(ts.clone(), "Current unix timestamp".into(), ts));
    }

    // in N units / N units from now / N units ago
    static RE_REL: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(?:in\s+)?([+-]?\d+(?:\.\d+)?)\s*",
            r"(seconds?|secs?|s|minutes?|mins?|m|hours?|hrs?|h|",
            r"days?|d|weeks?|w|months?|mo|years?|y|yrs?)\s*",
            r"(ago|from now|later)?\s*$",
        ))
        .unwrap()
    });
    if let Some(c) = RE_REL.captures(&lower) {
        let n: f64 = c.get(1)?.as_str().parse().ok()?;
        let unit = c.get(2)?.as_str();
        let dir = c.get(3).map(|m| m.as_str()).unwrap_or("from now");
        let secs = relative_secs(n, unit)?;
        let delta = if dir == "ago" {
            Duration::milliseconds((-secs * 1000.0) as i64)
        } else {
            Duration::milliseconds((secs * 1000.0) as i64)
        };
        let then = now + delta;
        let s = then.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        return Some(result_calc(s.clone(), format!("{n} {unit} {dir}"), s));
    }

    // days until / days since YYYY-MM-DD
    static RE_UNTIL: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(?:days?\s+(?:until|to|till|before|after|since)\s+)?(\d{4}-\d{2}-\d{2})\s*$",
        )
        .unwrap()
    });
    static RE_UNTIL2: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*days?\s+(until|to|till|before|after|since)\s+(\d{4}-\d{2}-\d{2})\s*$")
            .unwrap()
    });
    if let Some(c) = RE_UNTIL2.captures(&lower) {
        let date = NaiveDate::parse_from_str(c.get(2)?.as_str(), "%Y-%m-%d").ok()?;
        let today = now.date_naive();
        let days = (date - today).num_days();
        let title = format!("{days} days");
        return Some(result_calc(
            title.clone(),
            format!("{} → {date}", c.get(1)?.as_str()),
            days.to_string(),
        ));
    }
    if let Some(c) = RE_UNTIL.captures(&lower) {
        if lower.contains('-') && lower.len() == 10 {
            let date = NaiveDate::parse_from_str(c.get(1)?.as_str(), "%Y-%m-%d").ok()?;
            let days = (date - now.date_naive()).num_days();
            let title = format!("{days} days");
            return Some(result_calc(
                title.clone(),
                format!("from today to {date}"),
                days.to_string(),
            ));
        }
    }

    // parse datetime strings
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d",
        "%d/%m/%Y",
        "%m/%d/%Y",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(q, fmt) {
            let s = format!(
                "{} · unix {}",
                dt.format("%A, %d %B %Y %H:%M"),
                dt.and_utc().timestamp()
            );
            return Some(result_calc(
                dt.format("%Y-%m-%d %H:%M:%S").to_string(),
                s,
                dt.and_utc().timestamp().to_string(),
            ));
        }
        if let Ok(d) = NaiveDate::parse_from_str(q, fmt) {
            let s = d.format("%A, %d %B %Y").to_string();
            return Some(result_calc(
                s.clone(),
                format!("date · day {}", d.weekday()),
                s,
            ));
        }
    }

    // week number
    if matches!(lower.as_str(), "week" | "week number" | "iso week") {
        let w = now.iso_week();
        let s = format!("Week {} · {}", w.week(), now.format("%Y"));
        return Some(result_calc(
            s.clone(),
            "ISO week".into(),
            w.week().to_string(),
        ));
    }

    // day of year
    if matches!(lower.as_str(), "day of year" | "doy") {
        let d = now.ordinal();
        return Some(result_calc(
            format!("Day {d}"),
            now.format("%Y-%m-%d").to_string(),
            d.to_string(),
        ));
    }

    let _ = now.hour(); // keep Timelike import used
    None
}
