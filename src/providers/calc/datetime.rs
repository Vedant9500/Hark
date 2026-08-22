use super::util::{card_result, relative_secs};
use crate::providers::SearchResult;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Utc};
use once_cell::sync::Lazy;
use regex::Regex;

/// Readable 3-line clock for the card's right panel: `5:00pm` / `Saturday` /
/// `15 Aug 2026`. Works for local/utc `DateTime` and naive timestamps.
fn fmt_readable_time(ts: &(impl Datelike + Timelike)) -> String {
    format!(
        "{}\n{}\n{} {} {}",
        fmt_time_only(ts),
        weekday_name(ts.weekday()),
        ts.day(),
        MONTH_ABBR[(ts.month() - 1) as usize],
        ts.year(),
    )
}

/// 12-hour clock time only: `5:00pm`.
fn fmt_time_only(ts: &impl Timelike) -> String {
    let hr = match ts.hour() % 12 {
        0 => 12,
        h => h,
    };
    let ampm = if ts.hour() < 12 { "am" } else { "pm" };
    format!("{hr}:{:02}{ampm}", ts.minute())
}

/// Relative display that answers what was asked: same day → time only; next /
/// prev day → time + `Tomorrow`/`Yesterday`; within a week → time + weekday;
/// further out → full readout.
fn fmt_relative(then: &(impl Datelike + Timelike), now: &impl Datelike) -> String {
    let day = date_of(then);
    let today = date_of(now);
    let diff = (day - today).num_days();
    if diff == 0 {
        fmt_time_only(then)
    } else if diff == 1 {
        format!("{}\nTomorrow", fmt_time_only(then))
    } else if diff == -1 {
        format!("{}\nYesterday", fmt_time_only(then))
    } else if diff.abs() <= 6 {
        format!("{}\n{}", fmt_time_only(then), weekday_name(then.weekday()))
    } else {
        fmt_readable_time(then)
    }
}

/// Readable 2-line date: `Saturday` / `15 Aug 2026`.
fn fmt_readable_date(d: &impl Datelike) -> String {
    format!(
        "{}\n{} {} {}",
        weekday_name(d.weekday()),
        d.day(),
        MONTH_ABBR[(d.month() - 1) as usize],
        d.year(),
    )
}

/// Day-count answer: `Today`/`Tomorrow`/`Yesterday` for ±1, else `N days` with
/// a weekday line (`5 days` / `Saturday`).
fn fmt_days_until(days: i64, date: &impl Datelike) -> (String, String) {
    match days {
        0 => ("Today".into(), "Today".into()),
        1 => ("Tomorrow".into(), "Tomorrow".into()),
        -1 => ("Yesterday".into(), "Yesterday".into()),
        n => {
            let count = format!("{n} days");
            (
                count.clone(),
                format!("{count}\n{}", weekday_name(date.weekday())),
            )
        }
    }
}

const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn date_of(d: &impl Datelike) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), d.month(), d.day()).expect("valid date")
}

/// `27 august 2026` / `26 aug` → date. Missing year defaults to the current
/// year, bumped forward when the date already passed (looks like an upcoming
/// weekday question).
fn parse_text_date(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    static RE_TEXT_DATE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*(\d{1,2})\s+(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\s*(\d{4})?\s*$")
            .unwrap()
    });
    let c = RE_TEXT_DATE.captures(s)?;
    let day: u32 = c.get(1)?.as_str().parse().ok()?;
    let month = month_idx(c.get(2)?.as_str())?;
    let had_year = c.get(3).is_some();
    let year: i32 = c
        .get(3)
        .map(|y| y.as_str().parse().ok())
        .unwrap_or(Some(today.year()))?;
    let d = NaiveDate::from_ymd_opt(year, month, day)?;
    if !had_year && d < today {
        NaiveDate::from_ymd_opt(year + 1, month, day)
    } else {
        Some(d)
    }
}

/// Days in `(year, month)`, leap-year aware: first of the next month minus a day.
/// None past chrono's representable range (user-typed extreme years).
fn days_in_month(year: i32, month: u32) -> Option<u32> {
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    Some(
        NaiveDate::from_ymd_opt(y, m, 1)?
            .checked_sub_signed(Duration::days(1))?
            .day(),
    )
}

/// Break the span `a → b` (a ≤ b) into whole years, months, days, plus total
/// days. Walks calendar-safe increments so anchors like `2020-02-29` or
/// `2023-01-31` stay valid: `with_year`/`with_month` return None on impossible
/// dates instead of clamping, so the day is clamped explicitly here.
fn ymd_between(a: NaiveDate, b: NaiveDate) -> (i64, i64, i64, i64) {
    // Advance by `months` steps (12 = one year), clamping day-of-month
    // (`Feb 29 → Feb 28`, `Jan 31 → Feb 28`). None only past chrono's range.
    fn advance(cur: NaiveDate, months: i32) -> Option<NaiveDate> {
        let total = cur.year() * 12 + cur.month() as i32 - 1 + months;
        let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
        NaiveDate::from_ymd_opt(y, m, cur.day().min(days_in_month(y, m)?))
    }

    let (mut y, mut m) = (0i64, 0i64);
    let mut cur = a;
    while let Some(next) = advance(cur, 12) {
        if next > b {
            break;
        }
        cur = next;
        y += 1;
    }
    while let Some(next) = advance(cur, 1) {
        if next > b {
            break;
        }
        cur = next;
        m += 1;
    }
    (y, m, (b - cur).num_days(), (b - a).num_days())
}

fn fmt_span(y: i64, m: i64, d: i64) -> String {
    let mut parts = Vec::new();
    if y > 0 {
        parts.push(format!("{y} year{}", if y == 1 { "" } else { "s" }));
    }
    if m > 0 {
        parts.push(format!("{m} month{}", if m == 1 { "" } else { "s" }));
    }
    if d > 0 || parts.is_empty() {
        parts.push(format!("{d} day{}", if d == 1 { "" } else { "s" }));
    }
    parts.join(" ")
}

fn month_idx(abbr: &str) -> Option<u32> {
    match abbr.get(..3)?.to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

/// Parse a plain numeric date in the accepted datetime formats. Day-first
/// (`dd/mm/yyyy`, `dd-mm-yy`) is tried before US `mm/dd/yyyy`; `%y` uses the
/// standard 69-99 → 19xx / 00-68 → 20xx pivot for 2-digit years.
fn numeric_naive_date(s: &str) -> Option<NaiveDate> {
    // 2-digit-year variants before %Y: chrono `%Y` reads `05` as year 5 and
    // `11-03-05` as year 11, so `%y` must win for the same separator shape.
    for fmt in [
        "%d-%m-%y", "%Y-%m-%d", "%d/%m/%y", "%d/%m/%Y", "%m/%d/%Y", "%d-%m-%Y",
    ] {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            return Some(d);
        }
    }
    None
}

fn weekday_name(w: chrono::Weekday) -> &'static str {
    match w {
        chrono::Weekday::Mon => "Monday",
        chrono::Weekday::Tue => "Tuesday",
        chrono::Weekday::Wed => "Wednesday",
        chrono::Weekday::Thu => "Thursday",
        chrono::Weekday::Fri => "Friday",
        chrono::Weekday::Sat => "Saturday",
        chrono::Weekday::Sun => "Sunday",
    }
}

pub(crate) fn try_datetime(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase();
    let now = Local::now();

    if matches!(lower.as_str(), "now" | "time" | "date" | "today") {
        let s = now.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        return Some(card_result(
            s.clone(),
            format!("Local now · unix {}", now.timestamp()),
            s.clone(),
            q.trim().to_string(),
            "local time",
            fmt_readable_time(&now),
            "result",
        ));
    }

    if lower == "utc" || lower == "now utc" {
        let utc = Utc::now();
        let s = utc.format("%Y-%m-%d %H:%M:%S UTC").to_string();
        return Some(card_result(
            s.clone(),
            "UTC now".into(),
            s.clone(),
            q.trim().to_string(),
            "utc",
            fmt_readable_time(&utc),
            "result",
        ));
    }

    if lower == "tomorrow" {
        let d = now + Duration::days(1);
        let s = d.format("%Y-%m-%d (%A)").to_string();
        return Some(card_result(
            s.clone(),
            "Tomorrow".into(),
            s.clone(),
            "tomorrow".into(),
            "date",
            fmt_readable_date(&d),
            "result",
        ));
    }

    if lower == "yesterday" {
        let d = now - Duration::days(1);
        let s = d.format("%Y-%m-%d (%A)").to_string();
        return Some(card_result(
            s.clone(),
            "Yesterday".into(),
            s.clone(),
            "yesterday".into(),
            "date",
            fmt_readable_date(&d),
            "result",
        ));
    }

    // unix timestamp
    static RE_UNIX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^\s*(?:unix|epoch|timestamp)\s+([+-]?\d+)\s*$").unwrap());
    if let Some(c) = RE_UNIX.captures(&lower) {
        let ts: i64 = c.get(1)?.as_str().parse().ok()?;
        let dt = chrono::DateTime::from_timestamp(ts, 0)?;
        let local = dt.with_timezone(&Local);
        let s = local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        return Some(card_result(
            s.clone(),
            format!("unix {ts}"),
            s.clone(),
            format!("unix {ts}"),
            "unix",
            fmt_readable_time(&local),
            "result",
        ));
    }

    // bare large epoch
    if let Ok(ts) = lower.parse::<i64>() {
        if (1_000_000_000..4_000_000_000).contains(&ts) {
            let dt = chrono::DateTime::from_timestamp(ts, 0)?;
            let local = dt.with_timezone(&Local);
            let s = local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
            return Some(card_result(
                s.clone(),
                format!("unix {ts}"),
                s.clone(),
                format!("{ts}"),
                "unix",
                fmt_readable_time(&local),
                "result",
            ));
        }
    }

    // "to unix" / "unix now"
    if matches!(lower.as_str(), "unix" | "epoch" | "to unix" | "unix now") {
        let ts = now.timestamp().to_string();
        return Some(card_result(
            ts.clone(),
            "Current unix timestamp".into(),
            ts.clone(),
            "now".into(),
            "unix",
            ts,
            "result",
        ));
    }

    // in N units / N units from now / N units ago.
    // Bare `m` is meters, not minutes (minutes need `min`/`mins`/`minute`), so
    // `100m` is not hijacked into a "100 m from now" timestamp.
    static RE_REL: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(in\s+)?",
            r"((?:[+-]?\d+(?:\.\d+)?\s*(?:seconds?|secs?|s|",
            r"minutes?|mins?|hours?|hrs?|h|days?|d|weeks?|w|",
            r"months?|mo|years?|y|yrs?)\s*)+)",
            r"(ago|from now|later)?\s*$",
        ))
        .unwrap()
    });
    static RE_REL_TOK: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)([+-]?\d+(?:\.\d+)?)\s*",
            r"(seconds?|secs?|s|minutes?|mins?|hours?|hrs?|h|",
            r"days?|d|weeks?|w|months?|mo|years?|y|yrs?)",
        ))
        .unwrap()
    });
    if let Some(c) = RE_REL.captures(&lower) {
        let has_in = c.get(1).is_some();
        let dir_explicit = c.get(3).is_some();
        let dir = c.get(3).map(|m| m.as_str()).unwrap_or("from now");
        let mut total_secs = 0.0_f64;
        let mut ntok = 0;
        for t in RE_REL_TOK.captures_iter(c.get(2)?.as_str()) {
            let n: f64 = t.get(1)?.as_str().parse().ok()?;
            let unit = t.get(2)?.as_str();
            total_secs += relative_secs(n, unit)?;
            ntok += 1;
        }
        // Multi-unit bare forms ("10h 30min") are durations, not timestamps;
        // only treat them as relative times with an explicit `in `/direction.
        if ntok >= 2 && !has_in && !dir_explicit {
            return None;
        }
        // chrono Duration is i64 milliseconds; a saturating `f64 as i64` cast
        // on huge inputs (e.g. 1e20 years) silently yields wrong dates.
        // Clamp to ±100 years so the cast is exact and always finite.
        const MAX_RELATIVE_SECS: f64 = 100.0 * 31_556_952.0;
        let secs = total_secs.clamp(-MAX_RELATIVE_SECS, MAX_RELATIVE_SECS);
        let delta = if dir == "ago" {
            Duration::milliseconds((-secs * 1000.0) as i64)
        } else {
            Duration::milliseconds((secs * 1000.0) as i64)
        };
        let then = now + delta;
        let s = then.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        let shown = c.get(2)?.as_str().trim();
        return Some(card_result(
            s.clone(),
            format!("{shown} {dir}"),
            s.clone(),
            shown.to_string(),
            "relative",
            fmt_relative(&then, &now),
            "result",
        ));
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
        let (title, right) = fmt_days_until(days, &date);
        return Some(card_result(
            title,
            format!("{} → {date}", c.get(1)?.as_str()),
            days.to_string(),
            date.to_string(),
            "date",
            right,
            "result",
        ));
    }
    if let Some(c) = RE_UNTIL.captures(&lower) {
        if lower.contains('-') && lower.len() == 10 {
            let date = NaiveDate::parse_from_str(c.get(1)?.as_str(), "%Y-%m-%d").ok()?;
            let days = (date - now.date_naive()).num_days();
            let (title, right) = fmt_days_until(days, &date);
            return Some(card_result(
                title,
                format!("from today to {date}"),
                days.to_string(),
                date.to_string(),
                "date",
                right,
                "result",
            ));
        }
    }

    // what day is <date> / day on <date> / day <date> / on <date> → weekday.
    static RE_DAY_LOOKUP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*(?:what\s+day(?:\s+is)?|day)\s+(?:on\s+)?(.+?)\s*$").unwrap()
    });
    static RE_ON: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^\s*on\s+(.+?)\s*$").unwrap());
    let day_query = RE_DAY_LOOKUP
        .captures(&lower)
        .or_else(|| RE_ON.captures(&lower))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str());
    if let Some(rem) = day_query {
        if let Some(date) =
            parse_text_date(rem, now.date_naive()).or_else(|| numeric_naive_date(rem))
        {
            let wd = weekday_name(date.weekday());
            let shown = q.trim().to_string();
            return Some(card_result(
                date.format("%Y-%m-%d").to_string(),
                shown.clone(),
                wd.to_string(),
                shown,
                "date",
                wd.into(),
                "result",
            ));
        }
    }

    // age / date diff: `age 1998-03-15`, `age 11/03/2005`, `age 11-03-05`,
    // `1998-03-15 to now`, `11/03/2005 to 20/03/2005`
    static RE_AGE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^age\s+([\d]{1,4}[-/][\d]{1,2}[-/][\d]{2,4})\s*$").unwrap());
    if let Some(c) = RE_AGE.captures(&lower) {
        let birth = numeric_naive_date(c.get(1)?.as_str())?;
        let today = now.date_naive();
        if birth > today {
            return None;
        }
        let (y, mo, d, days) = ymd_between(birth, today);
        let title = fmt_span(y, mo, d);
        return Some(card_result(
            title.clone(),
            format!("{} → today", c.get(1)?.as_str()),
            format!("{} days · born {}", days, c.get(1)?.as_str()),
            c.get(1)?.as_str().to_string(),
            "age",
            title,
            "result",
        ));
    }

    static RE_DIFF: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^([\d]{1,4}[-/][\d]{1,2}[-/][\d]{2,4})\s+to\s+(now|today|[\d]{1,4}[-/][\d]{1,2}[-/][\d]{2,4})\s*$",
        )
        .unwrap()
    });
    if let Some(c) = RE_DIFF.captures(&lower) {
        let a = numeric_naive_date(c.get(1)?.as_str())?;
        let b = match c.get(2)?.as_str() {
            "now" | "today" => now.date_naive(),
            s => numeric_naive_date(s)?,
        };
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let (y, mo, d, days) = ymd_between(start, end);
        let title = fmt_span(y, mo, d);
        return Some(card_result(
            title.clone(),
            format!("{} → {}", c.get(1)?.as_str(), c.get(2)?.as_str()),
            format!("{} days between", days),
            c.get(1)?.as_str().to_string(),
            "date diff",
            title,
            "result",
        ));
    }

    // parse datetime strings; 2-digit-year variants before %Y (chrono %Y
    // reads `05` as year 5 and `11-03-05` as year 11)
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%d-%m-%y",
        "%Y-%m-%d",
        "%d/%m/%y",
        "%d/%m/%Y",
        "%m/%d/%Y",
        "%d-%m-%Y",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(q, fmt) {
            let s = format!(
                "{} · unix {}",
                dt.format("%A, %d %B %Y %H:%M"),
                dt.and_utc().timestamp()
            );
            return Some(card_result(
                dt.format("%Y-%m-%d %H:%M:%S").to_string(),
                s,
                dt.and_utc().timestamp().to_string(),
                q.trim().to_string(),
                "date",
                fmt_readable_time(&dt),
                "result",
            ));
        }
        if let Ok(d) = NaiveDate::parse_from_str(q, fmt) {
            let s = d.format("%A, %d %B %Y").to_string();
            return Some(card_result(
                s.clone(),
                format!("date · day {}", d.weekday()),
                s.clone(),
                q.trim().to_string(),
                "date",
                fmt_readable_date(&d),
                "result",
            ));
        }
    }

    // week number
    if matches!(lower.as_str(), "week" | "week number" | "iso week") {
        let w = now.iso_week();
        let s = format!("Week {} · {}", w.week(), now.format("%Y"));
        return Some(card_result(
            s.clone(),
            "ISO week".into(),
            w.week().to_string(),
            "week".into(),
            "iso week",
            s,
            "result",
        ));
    }

    // day of year
    if matches!(lower.as_str(), "day of year" | "doy") {
        let d = now.ordinal();
        return Some(card_result(
            format!("Day {d}"),
            now.format("%Y-%m-%d").to_string(),
            d.to_string(),
            "day of year".into(),
            "day",
            format!("{d}"),
            "result",
        ));
    }

    let _ = now.hour(); // keep Timelike import used
    None
}

#[cfg(test)]
mod datetime_tests {
    use super::*;

    #[test]
    fn relative_display_escalates_by_distance() {
        // 2026-08-15 is a Saturday.
        let now = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        let t = |day: u32, hour: u32| {
            NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2026, 8, day).unwrap(),
                chrono::NaiveTime::from_hms_opt(hour, 0, 0).unwrap(),
            )
        };
        assert_eq!(fmt_relative(&t(15, 12), &now), "12:00pm");
        assert_eq!(fmt_relative(&t(16, 12), &now), "12:00pm\nTomorrow");
        assert_eq!(fmt_relative(&t(14, 12), &now), "12:00pm\nYesterday");
        assert_eq!(fmt_relative(&t(18, 12), &now), "12:00pm\nTuesday");
        assert_eq!(
            fmt_relative(&t(23, 12), &now),
            "12:00pm\nSunday\n23 Aug 2026"
        );
    }

    #[test]
    fn days_until_uses_words_for_near_dates() {
        let d = |m: u32, day: u32| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        assert_eq!(
            fmt_days_until(0, &d(8, 15)),
            ("Today".into(), "Today".into())
        );
        assert_eq!(
            fmt_days_until(1, &d(8, 16)),
            ("Tomorrow".into(), "Tomorrow".into())
        );
        assert_eq!(
            fmt_days_until(-1, &d(8, 14)),
            ("Yesterday".into(), "Yesterday".into())
        );
        assert_eq!(
            fmt_days_until(5, &d(8, 20)),
            ("5 days".into(), "5 days\nThursday".into())
        );
    }

    #[test]
    fn huge_relative_input_is_clamped() {
        let now = Local::now().date_naive();
        // 1e20 years would silently saturate the i64-millisecond cast; the
        // result must be clamped to ~100 years and stay a valid date.
        for q in [
            "100000000000000000000 years",
            "-100000000000000000000 years ago",
        ] {
            let r = try_datetime(q).expect("clamped result");
            let d = NaiveDate::parse_from_str(&r.title[..10], "%Y-%m-%d").unwrap();
            let years = (d - now).num_days() as f64 / 365.25;
            assert!(years.abs() <= 110.0, "{q} → {years:.0} years off");
        }
    }

    #[test]
    fn bare_m_is_meters_not_minutes() {
        // `100m` is 100 meters — no "100 m from now" hijack.
        assert!(try_datetime("100m").is_none());
        assert!(try_datetime("30m from now").is_none());
        assert!(try_datetime("in 30m").is_none());
        // Minutes still resolve via min/mins/minute/minutes.
        assert!(try_datetime("45 min from now").is_some());
        assert!(try_datetime("in 30 min").is_some());
    }

    #[test]
    fn multi_unit_relative_requires_signal() {
        // Bare multi-unit is a duration card, not a timestamp.
        assert!(try_datetime("10h 30min").is_none());
        // Explicit `in` or direction → future timestamp.
        for q in ["in 1h 30min", "1h 30 min from now", "in 2h"] {
            let r = try_datetime(q).expect(q);
            let d = NaiveDate::parse_from_str(&r.title[..10], "%Y-%m-%d").unwrap();
            assert!(d >= Local::now().date_naive(), "{q} → past: {d}");
        }
        let r = try_datetime("1h 30min ago").expect("ago");
        let d = NaiveDate::parse_from_str(&r.title[..10], "%Y-%m-%d").unwrap();
        assert!(d <= Local::now().date_naive(), "ago → future: {d}");
    }

    #[test]
    fn all_datetime_results_render_cards() {
        let cases: &[(&str, &str)] = &[
            ("now", "local time"),
            ("time", "local time"),
            ("date", "local time"),
            ("today", "local time"),
            ("utc", "utc"),
            ("now utc", "utc"),
            ("tomorrow", "date"),
            ("yesterday", "date"),
            ("unix 1735000000", "unix"),
            ("1735000000", "unix"),
            ("to unix", "unix"),
            ("in 1h 30min", "relative"),
            ("1h 30min ago", "relative"),
            ("days until 2026-08-20", "date"),
            ("2026-08-20", "date"),
            ("15/08/2026", "date"),
            ("week", "iso week"),
            ("day of year", "day"),
            ("day on 27 august 2026", "date"),
            ("on 26 aug", "date"),
        ];
        for (q, badge) in cases {
            let r = try_datetime(q).expect(q);
            let conv = r.conversion.expect("card");
            assert_eq!(conv.right_badge, "result", "{q}");
            assert_eq!(conv.left_badge, *badge, "{q}");
        }
    }

    #[test]
    fn right_title_is_readable_multiline() {
        let r = try_datetime("unix 1735000000").expect("unix");
        let conv = r.conversion.as_ref().expect("card");
        let lines: Vec<&str> = conv.right_title.split('\n').collect();
        assert_eq!(lines.len(), 3, "{:?}", conv.right_title);
        assert!(lines[0].contains(':'), "time line: {}", lines[0]);
        assert!(lines[2].contains("2024"), "year line: {}", lines[2]);

        let r = try_datetime("15/08/2026").expect("parsed date");
        let conv = r.conversion.as_ref().expect("card");
        let lines: Vec<&str> = conv.right_title.split('\n').collect();
        assert_eq!(lines.len(), 2, "{:?}", conv.right_title);
        assert_eq!(lines[0], "Saturday");
        assert_eq!(lines[1], "15 Aug 2026");
    }

    #[test]
    fn day_lookup_weekday() {
        let r = try_datetime("day on 27 august 2026").expect("lookup");
        assert_eq!(r.title, "2026-08-27");
        let conv = r.conversion.expect("card");
        assert_eq!(conv.left_badge, "date");
        assert_eq!(conv.right_title, "Thursday");
        assert!(matches!(&r.action, crate::providers::Action::Copy(s) if s == "Thursday"));
        let r = try_datetime("what day is 26 aug").expect("lookup");
        assert_eq!(r.conversion.expect("card").right_title, "Wednesday");
        let r = try_datetime("on 26 aug").expect("lookup");
        assert_eq!(r.conversion.expect("card").right_title, "Wednesday");
        let r = try_datetime("day on 26 dec").expect("lookup");
        assert_eq!(r.conversion.expect("card").right_title, "Saturday");
        let r = try_datetime("day on 27/08/2026").expect("numeric lookup");
        assert_eq!(r.conversion.expect("card").right_title, "Thursday");
        // Past month without year rolls to next year.
        let r = try_datetime("day on 26 feb").expect("rollover");
        assert_eq!(r.conversion.expect("card").right_title, "Friday");
        // Unparseable remainder falls through, not an early error.
        assert!(try_datetime("day of year").is_some());
    }

    #[test]
    fn span_leap_day_anchor_counts_years() {
        // 2020-02-29 → 2024-02-29: with_year(2021) on a Feb-29 anchor yields
        // None, which stalled the year walk at 0 before the clamp fix.
        let a = NaiveDate::from_ymd_opt(2020, 2, 29).unwrap();
        let b = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        let (y, m, d, total) = ymd_between(a, b);
        assert_eq!((y, m), (4, 0), "leap-day anchor must walk 4 years");
        assert_eq!(d, 1, "clamped Feb 28 leaves one day");
        assert_eq!(total, 1461);
        assert_eq!(fmt_span(y, m, d), "4 years 1 day");
    }

    #[test]
    fn span_month_end_anchor_counts_months() {
        // 2023-01-31 → 2023-03-01: with_month(2) on day 31 yields None and
        // killed the month walk; now Jan 31 clamps to Feb 28 (+1 day).
        let a = NaiveDate::from_ymd_opt(2023, 1, 31).unwrap();
        let b = NaiveDate::from_ymd_opt(2023, 3, 1).unwrap();
        let (y, m, d, _) = ymd_between(a, b);
        assert_eq!((y, m, d), (0, 1, 1));
        // Day-31 anchors must not skip months either: Jan 31 → Apr 30 walks
        // Feb/Mar/Apr via clamped days instead of stalling at 0.
        let a = NaiveDate::from_ymd_opt(2021, 1, 31).unwrap();
        let b = NaiveDate::from_ymd_opt(2021, 4, 30).unwrap();
        let (y, m, _, total) = ymd_between(a, b);
        assert_eq!((y, m), (0, 3));
        assert_eq!(total, (b - a).num_days());
        // Plain mid-month spans are unchanged.
        let a = NaiveDate::from_ymd_opt(1998, 3, 15).unwrap();
        let b = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        assert_eq!(ymd_between(a, b), (28, 5, 0, (b - a).num_days()));
    }

    #[test]
    fn age_and_date_diff() {
        let r = try_datetime("age 1998-03-15").expect("age");
        assert!(r.conversion.as_ref().unwrap().left_badge == "age", "{r:?}");
        // 1998-03-15 → 2026-08-15 = 28y 5m.
        assert!(r.title.starts_with("28 years 5 months"), "{}", r.title);
        let r = try_datetime("1998-03-15 to 2026-08-15").expect("diff");
        assert_eq!(r.title, "28 years 5 months");
        assert_eq!(r.conversion.as_ref().unwrap().left_badge, "date diff");
        // Reverse order normalizes.
        let r = try_datetime("2026-08-15 to 1998-03-15").expect("reversed");
        assert_eq!(r.title, "28 years 5 months");
        // `to now` resolves against the clock.
        let r = try_datetime("1998-03-15 to now").expect("to now");
        assert!(r.title.starts_with("28 years"), "{}", r.title);
        // Single dates still fall through to the plain date card.
        let r = try_datetime("2026-08-15").expect("bare date");
        assert_eq!(r.conversion.as_ref().unwrap().left_badge, "date");
    }

    #[test]
    fn age_and_date_diff_alternate_formats() {
        // dd/mm/yyyy, dd/mm/yy, dd-mm-yyyy, dd-mm-yy.
        for birth in ["11/03/2005", "11/03/05", "11-03-2005", "11-03-05"] {
            let r = try_datetime(&format!("age {birth}")).unwrap_or_else(|| panic!("age {birth}"));
            assert!(r.title.starts_with("21 years"), "{}: {}", birth, r.title);
        }
        let r = try_datetime("11/03/2005 to 20/03/2005").expect("diff slashes");
        assert_eq!(r.title, "9 days");
        let r = try_datetime("11-03-2005 to 11-04-2005").expect("diff dashes");
        assert_eq!(r.title, "1 month");
        let r = try_datetime("11/03/05 to now").expect("diff short to now");
        assert!(r.title.starts_with("21 years"), "{}", r.title);
        let r = try_datetime("11/03/2005 to 20/03/2026").expect("diff long span");
        assert_eq!(r.title, "21 years 9 days");
        // Unambiguous day-first: 11/03/2005 is 11 March, never 3 Nov.
        let r = try_datetime("age 11/03/2005").expect("day-first");
        assert!(r.title.starts_with("21 years 5 months"), "{}", r.title);
    }
}
