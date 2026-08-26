use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use chrono::{Local, NaiveDateTime, NaiveTime, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use once_cell::sync::Lazy;
use regex::Regex;

/// Timezone compare / convert.
///
/// Examples:
/// - `now in tokyo`, `time in new york`
/// - `15:00 here to london`, `3pm here in tokyo`
/// - `15:00 in mumbai`, `3pm in paris` (local → city)
/// - `15:00 in london to here`, `3pm tokyo to here` (city → local)
/// - `16:00 cet to ist`, `4pm est to pst`
pub(crate) fn try_timezone(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase().trim().to_string();

    // now in <place> / time in <place>
    static RE_NOW_TZ: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^\s*(?:now|time)\s+(?:in|at)\s+(.+?)\s*$").unwrap());
    if let Some(c) = RE_NOW_TZ.captures(&lower) {
        let place = c.get(1)?.as_str().trim();
        let (tz, label) = resolve_place(place)?;
        let local = Local::now();
        let dt = Utc::now().with_timezone(&tz);
        let left_title = format_ampm_compact(local.hour(), local.minute());
        let right_title = dt.format("%H:%M").to_string();
        let copy = format!("{} {}", dt.format("%H:%M"), label);
        return Some(tz_result(
            &format!("now→{label}"),
            format!("{left_title} here"),
            format_ampm_badge(local.hour(), local.minute()),
            right_title,
            format!("{},{}", label, dt.format("%Z")),
            copy,
        ));
    }

    // TIME here|local (in|to|as) PLACE  — local → city
    static RE_HERE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?\s+",
            r"(here|local)\s+(?:in|to|as|->|→)\s+(.+?)\s*$",
        ))
        .unwrap()
    });
    // TIME in PLACE to here|local|PLACE — city → local (or city → city)
    // Also: TIME at PLACE to …
    static RE_IN_FROM_TO: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?\s+",
            r"(?:in|at)\s+(.+?)\s+(?:to|as|->|→)\s+(.+?)\s*$",
        ))
        .unwrap()
    });
    // TIME in|to PLACE — local → city (no "here" word). Requires am/pm OR :mm so bare
    // "15 tokyo" is not stolen.
    static RE_IN_CITY: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?\s+",
            r"(?:in|to)\s+(.+?)\s*$",
        ))
        .unwrap()
    });
    // Classic: TIME FROM to TO (optional am/pm; FROM/TO may be multi-word)
    static RE_TZ: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(\d{1,2}):(\d{2})(?::(\d{2}))?\s*(am|pm)?\s+",
            r"(.+?)\s+(?:to|in|as|->|→)\s+(.+?)\s*$",
        ))
        .unwrap()
    });
    // Compact no-colon: 4pm est to pst
    static RE_TZ_COMPACT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)\s+(.+?)\s+(?:to|in|as|->|→)\s+(.+?)\s*$",
        )
        .unwrap()
    });

    if let Some(c) = RE_HERE.captures(&lower) {
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            c.get(3).map(|m| m.as_str()),
            c.get(4).map(|m| m.as_str()),
        )?;
        let (from_tz, from_label) = local_as_tz()?;
        let to_token = c.get(6)?.as_str().trim();
        let (to_tz, to_label) = resolve_place(to_token)?;
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }

    if let Some(c) = RE_IN_FROM_TO.captures(&lower) {
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            c.get(3).map(|m| m.as_str()),
            c.get(4).map(|m| m.as_str()),
        )?;
        // Require a real clock signal: minutes and/or am/pm (avoid "15 in x to y")
        if c.get(2).is_none() && c.get(4).is_none() {
            return None;
        }
        let from_token = c.get(5)?.as_str().trim();
        let to_token = c.get(6)?.as_str().trim();
        let (from_tz, from_label) = resolve_place(from_token)?;
        let (to_tz, to_label) = resolve_place(to_token)?;
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }

    if let Some(c) = RE_IN_CITY.captures(&lower) {
        let ampm = c.get(4).map(|m| m.as_str());
        let has_minutes = c.get(2).is_some();
        // Need am/pm or :mm so we don't treat random "3 in foo" text as a clock.
        if ampm.is_none() && !has_minutes {
            // fall through
        } else {
            let (hour, minute, second) = parse_clock(
                c.get(1)?.as_str(),
                c.get(2).map(|m| m.as_str()),
                c.get(3).map(|m| m.as_str()),
                ampm,
            )?;
            let to_token = c.get(5)?.as_str().trim();
            if to_token.is_empty() {
                return None;
            }
            // "15:00 in here" is a no-op — skip
            if matches!(to_token, "here" | "local" | "system") {
                // fall through
            } else {
                let (from_tz, from_label) = local_as_tz()?;
                let (to_tz, to_label) = resolve_place(to_token)?;
                return build_tz_conversion(
                    hour,
                    minute,
                    second,
                    from_tz,
                    &from_label,
                    to_tz,
                    &to_label,
                );
            }
        }
    }

    if let Some(c) = RE_TZ_COMPACT.captures(&lower) {
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            None,
            c.get(3).map(|m| m.as_str()),
        )?;
        let from_token = c.get(4)?.as_str().trim();
        let to_token = c.get(5)?.as_str().trim();
        // Avoid re-matching "3pm here to london" (handled by RE_HERE)
        if matches!(from_token, "here" | "local") {
            // fall through to RE_TZ or fail
        } else {
            let (from_tz, from_label) = resolve_place(from_token)?;
            let (to_tz, to_label) = resolve_place(to_token)?;
            return build_tz_conversion(
                hour,
                minute,
                second,
                from_tz,
                &from_label,
                to_tz,
                &to_label,
            );
        }
    }

    let caps = RE_TZ.captures(&lower)?;
    let (hour, minute, second) = parse_clock(
        caps.get(1)?.as_str(),
        Some(caps.get(2)?.as_str()),
        caps.get(3).map(|m| m.as_str()),
        caps.get(4).map(|m| m.as_str()),
    )?;
    let from_raw = caps.get(5)?.as_str().trim();
    let to_raw = caps.get(6)?.as_str().trim();
    // "15:00 here to x" already handled; if we got here with here/local, resolve works.
    let (from_tz, from_label) = resolve_place(from_raw)?;
    let (to_tz, to_label) = resolve_place(to_raw)?;
    build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label)
}

/// Prefix match for incomplete city/zone: `lon` → London, `new yo` → New York
pub(crate) fn try_timezone_predict(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase();
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+",
            r"(here|local)\s+(?:in|to)\s+([a-zA-Z ]*?)\s*$",
        ))
        .unwrap()
    });
    static RE2: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+",
            r"(?:in|to)\s+([a-zA-Z ]*?)\s*$",
        ))
        .unwrap()
    });
    static RE3: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+",
            r"(?:in|at)\s+([a-zA-Z ]+?)\s+(?:to)\s+([a-zA-Z ]*?)\s*$",
        ))
        .unwrap()
    });

    if let Some(c) = RE.captures(&lower) {
        let prefix = c.get(5)?.as_str().trim();
        if prefix.len() < 2 {
            return None;
        }
        if resolve_place(prefix).is_some() {
            return None;
        }
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            None,
            c.get(3).map(|m| m.as_str()),
        )?;
        let (from_tz, from_label) = local_as_tz()?;
        let (to_tz, to_label) = predict_tz(prefix)?;
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }
    if let Some(c) = RE3.captures(&lower) {
        let from_prefix = c.get(5)?.as_str().trim();
        let to_prefix = c.get(6)?.as_str().trim();
        if from_prefix.len() < 2 {
            return None;
        }
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            None,
            c.get(3).map(|m| m.as_str()),
        )?;
        // Need clock signal
        if c.get(2).is_none() && c.get(3).is_none() {
            return None;
        }
        let (from_tz, from_label) =
            resolve_place(from_prefix).or_else(|| predict_tz(from_prefix))?;
        let (to_tz, to_label) = if to_prefix.is_empty() || to_prefix.len() < 2 {
            return None;
        } else {
            resolve_place(to_prefix).or_else(|| predict_tz(to_prefix))?
        };
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }
    if let Some(c) = RE2.captures(&lower) {
        let prefix = c.get(5)?.as_str().trim();
        if prefix.len() < 2 {
            return None;
        }
        // Require am/pm or minutes
        if c.get(2).is_none() && c.get(3).is_none() {
            return None;
        }
        if resolve_place(prefix).is_some() {
            return None;
        }
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            None,
            c.get(3).map(|m| m.as_str()),
        )?;
        let (from_tz, from_label) = local_as_tz()?;
        let (to_tz, to_label) = predict_tz(prefix)?;
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }
    None
}

/// Resolve a place/zone token (city, abbr, IANA, here/local).
pub(crate) fn resolve_place(token: &str) -> Option<(Tz, String)> {
    resolve_tz(token).or_else(|| predict_tz(token))
}

pub(crate) fn predict_tz(prefix: &str) -> Option<(Tz, String)> {
    let p = normalize_place_key(prefix);
    if p.is_empty() {
        return None;
    }
    // Offset tokens must never fuzzy-fall back to plain UTC
    // (alias "utc" starts_with-trap made any utc/gmt±token resolve as UTC).
    // Shape check, not parse check: invalid forms like `utc+25` or sign-less
    // `gmt8` must error out too, not silently become UTC.
    if let Some(rest) = p.strip_prefix("utc").or_else(|| p.strip_prefix("gmt")) {
        if rest
            .as_bytes()
            .first()
            .is_some_and(|b| b"+-_0123456789".contains(b))
        {
            return None;
        }
    }
    let mut hits: Vec<(&str, &str, &str, i32)> = CITY_ALIASES
        .iter()
        .filter(|(alias, _, _)| {
            let a = *alias;
            a.starts_with(&p)
                || p.starts_with(a)
                || a.replace('_', "").starts_with(&p.replace('_', ""))
        })
        .map(|(a, iana, label)| {
            let compact_a = a.replace('_', "");
            let compact_p = p.replace('_', "");
            let score = if *a == p || compact_a == compact_p {
                1000
            } else if a.starts_with(&p) || compact_a.starts_with(&compact_p) {
                500 - a.len() as i32
            } else {
                100
            };
            (*a, *iana, *label, score)
        })
        .collect();
    hits.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(b.0)));
    let (_, iana, label, _) = hits.first()?;
    let tz: Tz = iana.parse().ok()?;
    Some((tz, (*label).to_string()))
}

/// Shared city / zone alias table: (lookup key, IANA, display label).
/// Keys are normalized: lowercase, spaces → `_`.
const CITY_ALIASES: &[(&str, &str, &str)] = &[
    ("london", "Europe/London", "LONDON"),
    ("paris", "Europe/Paris", "PARIS"),
    ("berlin", "Europe/Berlin", "BERLIN"),
    ("amsterdam", "Europe/Amsterdam", "AMSTERDAM"),
    ("rome", "Europe/Rome", "ROME"),
    ("madrid", "Europe/Madrid", "MADRID"),
    ("stockholm", "Europe/Stockholm", "STOCKHOLM"),
    ("zurich", "Europe/Zurich", "ZURICH"),
    ("athens", "Europe/Athens", "ATHENS"),
    ("moscow", "Europe/Moscow", "MOSCOW"),
    ("istanbul", "Europe/Istanbul", "ISTANBUL"),
    ("tokyo", "Asia/Tokyo", "TOKYO"),
    ("osaka", "Asia/Tokyo", "OSAKA"),
    ("seoul", "Asia/Seoul", "SEOUL"),
    ("shanghai", "Asia/Shanghai", "SHANGHAI"),
    ("beijing", "Asia/Shanghai", "BEIJING"),
    ("hong_kong", "Asia/Hong_Kong", "HONG KONG"),
    ("hongkong", "Asia/Hong_Kong", "HONG KONG"),
    ("hk", "Asia/Hong_Kong", "HONG KONG"),
    ("singapore", "Asia/Singapore", "SINGAPORE"),
    ("dubai", "Asia/Dubai", "DUBAI"),
    ("uae", "Asia/Dubai", "DUBAI"),
    ("mumbai", "Asia/Kolkata", "MUMBAI"),
    ("delhi", "Asia/Kolkata", "DELHI"),
    ("new_delhi", "Asia/Kolkata", "DELHI"),
    ("kolkata", "Asia/Kolkata", "KOLKATA"),
    ("calcutta", "Asia/Kolkata", "KOLKATA"),
    ("bangalore", "Asia/Kolkata", "BANGALORE"),
    ("bengaluru", "Asia/Kolkata", "BANGALORE"),
    ("chennai", "Asia/Kolkata", "CHENNAI"),
    ("hyderabad", "Asia/Kolkata", "HYDERABAD"),
    ("pune", "Asia/Kolkata", "PUNE"),
    ("india", "Asia/Kolkata", "IST"),
    ("bangkok", "Asia/Bangkok", "BANGKOK"),
    ("jakarta", "Asia/Jakarta", "JAKARTA"),
    ("manila", "Asia/Manila", "MANILA"),
    ("karachi", "Asia/Karachi", "KARACHI"),
    ("pakistan", "Asia/Karachi", "PAKISTAN"),
    ("dhaka", "Asia/Dhaka", "DHAKA"),
    ("kathmandu", "Asia/Kathmandu", "KATHMANDU"),
    ("nepal", "Asia/Kathmandu", "NEPAL"),
    ("sydney", "Australia/Sydney", "SYDNEY"),
    ("melbourne", "Australia/Melbourne", "MELBOURNE"),
    ("perth", "Australia/Perth", "PERTH"),
    ("auckland", "Pacific/Auckland", "AUCKLAND"),
    ("new_york", "America/New_York", "NEW YORK"),
    ("newyork", "America/New_York", "NEW YORK"),
    ("nyc", "America/New_York", "NYC"),
    ("ny", "America/New_York", "NY"),
    ("chicago", "America/Chicago", "CHICAGO"),
    ("denver", "America/Denver", "DENVER"),
    ("los_angeles", "America/Los_Angeles", "LA"),
    ("losangeles", "America/Los_Angeles", "LA"),
    ("la", "America/Los_Angeles", "LA"),
    ("sf", "America/Los_Angeles", "SF"),
    ("san_francisco", "America/Los_Angeles", "SAN FRANCISCO"),
    ("sanfrancisco", "America/Los_Angeles", "SAN FRANCISCO"),
    ("seattle", "America/Los_Angeles", "SEATTLE"),
    ("toronto", "America/Toronto", "TORONTO"),
    ("vancouver", "America/Vancouver", "VANCOUVER"),
    ("sao_paulo", "America/Sao_Paulo", "SAO PAULO"),
    ("saopaulo", "America/Sao_Paulo", "SAO PAULO"),
    ("mexico_city", "America/Mexico_City", "MEXICO CITY"),
    ("mexico", "America/Mexico_City", "MEXICO"),
    ("cairo", "Africa/Cairo", "CAIRO"),
    ("johannesburg", "Africa/Johannesburg", "JOHANNESBURG"),
    ("lagos", "Africa/Lagos", "LAGOS"),
    ("nairobi", "Africa/Nairobi", "NAIROBI"),
    // abbreviations
    ("cet", "Europe/Paris", "CET"),
    ("cest", "Europe/Paris", "CEST"),
    ("eet", "Europe/Bucharest", "EET"),
    ("bst", "Europe/London", "BST"),
    ("ist", "Asia/Kolkata", "IST"),
    ("jst", "Asia/Tokyo", "JST"),
    ("kst", "Asia/Seoul", "KST"),
    ("hkt", "Asia/Hong_Kong", "HKT"),
    ("sgt", "Asia/Singapore", "SGT"),
    ("aest", "Australia/Sydney", "AEST"),
    ("aedt", "Australia/Sydney", "AEDT"),
    ("nzst", "Pacific/Auckland", "NZST"),
    ("est", "America/New_York", "EST"),
    ("edt", "America/New_York", "EDT"),
    ("et", "America/New_York", "ET"),
    ("eastern", "America/New_York", "EASTERN"),
    ("cst", "America/Chicago", "CST"),
    ("cdt", "America/Chicago", "CDT"),
    ("ct", "America/Chicago", "CT"),
    ("central", "America/Chicago", "CENTRAL"),
    ("mst", "America/Denver", "MST"),
    ("mdt", "America/Denver", "MDT"),
    ("mt", "America/Denver", "MT"),
    ("mountain", "America/Denver", "MOUNTAIN"),
    ("pst", "America/Los_Angeles", "PST"),
    ("pdt", "America/Los_Angeles", "PDT"),
    ("pt", "America/Los_Angeles", "PT"),
    ("pacific", "America/Los_Angeles", "PACIFIC"),
    ("gmt", "UTC", "GMT"),
    ("utc", "UTC", "UTC"),
];

fn normalize_place_key(token: &str) -> String {
    token
        .trim()
        .to_lowercase()
        .replace('-', "_")
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

pub(crate) fn parse_clock(
    hour_s: &str,
    minute_s: Option<&str>,
    second_s: Option<&str>,
    ampm: Option<&str>,
) -> Option<(u32, u32, u32)> {
    let mut hour: u32 = hour_s.parse().ok()?;
    let minute: u32 = minute_s.unwrap_or("0").parse().ok()?;
    let second: u32 = second_s.unwrap_or("0").parse().ok()?;
    if minute > 59 || second > 59 {
        return None;
    }
    if let Some(ap) = ampm {
        let ap = ap.to_lowercase();
        if hour == 0 || hour > 12 {
            return None;
        }
        if ap == "pm" && hour < 12 {
            hour += 12;
        }
        if ap == "am" && hour == 12 {
            hour = 0;
        }
    } else if hour > 23 {
        return None;
    }
    Some((hour, minute, second))
}

pub(crate) fn format_ampm_compact(hour: u32, minute: u32) -> String {
    let (h12, ap) = match hour {
        0 => (12, "am"),
        1..=11 => (hour, "am"),
        12 => (12, "pm"),
        _ => (hour - 12, "pm"),
    };
    if minute == 0 {
        format!("{h12}{ap}")
    } else {
        format!("{h12}:{minute:02}{ap}")
    }
}

pub(crate) fn format_ampm_badge(hour: u32, minute: u32) -> String {
    let (h12, ap) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{h12}:{minute:02} {ap}")
}

pub(crate) fn build_tz_conversion(
    hour: u32,
    minute: u32,
    second: u32,
    from_tz: Tz,
    from_label: &str,
    to_tz: Tz,
    to_label: &str,
) -> Option<SearchResult> {
    let time = NaiveTime::from_hms_opt(hour, minute, second)?;
    let today = Utc::now().with_timezone(&from_tz).date_naive();
    let naive = NaiveDateTime::new(today, time);
    let from_dt = naive.and_local_timezone(from_tz).single()?;
    let to_dt = from_dt.with_timezone(&to_tz);

    let left_title = format_ampm_compact(from_dt.hour(), from_dt.minute());
    let left_badge = format_ampm_badge(from_dt.hour(), from_dt.minute());
    let right_title = if second > 0 {
        to_dt.format("%H:%M:%S").to_string()
    } else {
        to_dt.format("%H:%M").to_string()
    };
    // Prefer 24h on right like Raycast screenshot, badge shows zone abbr
    let right_badge = format!("{},{}", to_label, to_dt.format("%Z"));

    let copy = format!("{} {}", to_dt.format("%H:%M"), to_label);
    // Left panel title like Raycast query echo when from local
    let left_display =
        if from_label.eq_ignore_ascii_case("LOCAL") || from_label.eq_ignore_ascii_case("HERE") {
            format!("{left_title} here")
        } else {
            format!("{left_title} {from_label}")
        };

    Some(tz_result(
        &format!("{from_label}→{to_label}:{hour}:{minute}"),
        left_display,
        left_badge,
        right_title,
        right_badge,
        copy,
    ))
}

pub(crate) fn tz_result(
    id_key: &str,
    left_title: String,
    left_badge: String,
    right_title: String,
    right_badge: String,
    copy: String,
) -> SearchResult {
    SearchResult {
        id: format!("tz:{id_key}"),
        title: right_title.clone(),
        subtitle: format!("{left_title} → {right_title} · {right_badge}"),
        kind: ResultKind::Conversion,
        score: 10_800,
        icon: Some("preferences-system-time".into()),
        action: Action::Copy(copy),
        conversion: Some(ConversionView {
            left_title,
            left_badge,
            right_title,
            right_badge,
        }),
        matched: None,
    }
}

pub(crate) fn resolve_tz(token: &str) -> Option<(Tz, String)> {
    let t = normalize_place_key(token);
    if t.is_empty() {
        return None;
    }
    // Full IANA name (Europe/London, Asia/Kolkata, …)
    if let Ok(tz) = t.parse::<Tz>() {
        return Some((tz, display_tz_label(&t)));
    }
    // Numeric offset tokens (utc+05:30, gmt-8, utc_8 — normalize maps '-'→'_')
    if let Some(off) = parse_utc_offset_token(&t) {
        let label = format_offset_label(off);
        return Some((tz_for_offset(off)?, label));
    }
    // Exact city / abbr from the shared table (incl. multi-word → underscores)
    if let Some((_, iana, label)) = CITY_ALIASES.iter().find(|(a, _, _)| *a == t) {
        let tz: Tz = iana.parse().ok()?;
        return Some((tz, (*label).to_string()));
    }
    // Compact form: "newyork" → "new_york"
    let compact = t.replace('_', "");
    if let Some((_, iana, label)) = CITY_ALIASES
        .iter()
        .find(|(a, _, _)| a.replace('_', "") == compact)
    {
        let tz: Tz = iana.parse().ok()?;
        return Some((tz, (*label).to_string()));
    }
    // Common aliases / abbreviations not listed above
    let iana = match t.as_str() {
        "utc" | "gmt" | "z" => "UTC",
        "cet" => "Europe/Paris", // CET/CEST with DST
        "cest" => "Europe/Paris",
        "eet" | "eest" => "Europe/Bucharest",
        "wet" | "west" => "Europe/Lisbon",
        "bst" => "Europe/London", // British Summer — London handles GMT/BST
        "ist" => "Asia/Kolkata",  // India Standard Time
        "ist-india" | "india" => "Asia/Kolkata",
        "jst" | "japan" => "Asia/Tokyo",
        "kst" | "korea" => "Asia/Seoul",
        "cst-china" | "china" => "Asia/Shanghai",
        "hkt" | "hongkong" => "Asia/Hong_Kong",
        "sgt" | "singapore" => "Asia/Singapore",
        "aest" | "aedt" | "sydney" => "Australia/Sydney",
        "acst" | "acdt" => "Australia/Adelaide",
        "awst" => "Australia/Perth",
        "nzst" | "nzdt" | "auckland" => "Pacific/Auckland",
        "est" | "edt" | "et" | "eastern" => "America/New_York",
        "cst" | "cdt" | "ct" | "central" => "America/Chicago",
        "mst" | "mdt" | "mt" | "mountain" => "America/Denver",
        "pst" | "pdt" | "pt" | "pacific" => "America/Los_Angeles",
        "akst" | "akdt" | "alaska" => "America/Anchorage",
        "hst" | "hawaii" => "Pacific/Honolulu",
        "ast" | "adt" | "atlantic" => "America/Halifax",
        "nst" | "ndt" => "America/St_Johns",
        "msk" | "moscow" => "Europe/Moscow",
        "trt" | "turkey" => "Europe/Istanbul",
        "gst" | "dubai" | "uae" => "Asia/Dubai",
        "pkt" | "pakistan" => "Asia/Karachi",
        "bst-bd" | "bangladesh" => "Asia/Dhaka",
        "npt" | "nepal" => "Asia/Kathmandu",
        "ict" | "bangkok" => "Asia/Bangkok",
        "wib" => "Asia/Jakarta",
        "pht" | "manila" => "Asia/Manila",
        "local" | "here" | "system" => {
            // Convert via local offset as fixed zone approximation using local now offset
            // Prefer Europe/… fallback: use chrono Local by mapping to Etc/GMT
            return local_as_tz();
        }
        // city shortcuts
        "london" => "Europe/London",
        "paris" => "Europe/Paris",
        "berlin" => "Europe/Berlin",
        "amsterdam" => "Europe/Amsterdam",
        "rome" => "Europe/Rome",
        "madrid" => "Europe/Madrid",
        "stockholm" => "Europe/Stockholm",
        "zurich" => "Europe/Zurich",
        "athens" => "Europe/Athens",
        "cairo" => "Africa/Cairo",
        "johannesburg" => "Africa/Johannesburg",
        "lagos" => "Africa/Lagos",
        "nairobi" => "Africa/Nairobi",
        "mumbai" | "delhi" | "kolkata" | "bangalore" | "chennai" | "hyderabad" => "Asia/Kolkata",
        "tokyo" => "Asia/Tokyo",
        "seoul" => "Asia/Seoul",
        "shanghai" | "beijing" => "Asia/Shanghai",
        "hong_kong" | "hk" => "Asia/Hong_Kong",
        "jakarta" => "Asia/Jakarta",
        "new_york" | "nyc" | "ny" => "America/New_York",
        "chicago" => "America/Chicago",
        "denver" => "America/Denver",
        "los_angeles" | "la" | "sf" | "san_francisco" => "America/Los_Angeles",
        "toronto" => "America/Toronto",
        "vancouver" => "America/Vancouver",
        "sao_paulo" | "saopaulo" => "America/Sao_Paulo",
        "mexico" | "mexico_city" => "America/Mexico_City",
        other => {
            // try parse as IANA with common capitalisation Asia/Kolkata style
            if other.contains('/') {
                other
            } else {
                return None;
            }
        }
    };
    let tz: Tz = iana.parse().ok()?;
    Some((tz, display_tz_label(token)))
}

pub(crate) fn display_tz_label(token: &str) -> String {
    token.trim().to_uppercase().replace('_', " ")
}

/// Half-hour zones chrono-tz can't express as Etc/GMT±N (seconds east of UTC).
/// DST-observing entries only resolve while their current offset matches —
/// verified at lookup time in `tz_for_offset`.
const HALF_HOUR_ZONES: &[(i32, &str)] = &[
    (19800, "Asia/Kolkata"),        // +5:30
    (20700, "Asia/Kathmandu"),      // +5:45
    (12600, "Asia/Tehran"),         // +3:30
    (16200, "Asia/Kabul"),          // +4:30
    (23400, "Asia/Yangon"),         // +6:30
    (-9000, "America/St_Johns"),    // -2:30 NDT (DST); std NST is -3:30 and fails check then
    (-34200, "Pacific/Marquesas"),  // -9:30 fixed
    (34200, "Australia/Darwin"),    // +9:30 fixed
    (31500, "Australia/Eucla"),     // +8:45
    (37800, "Australia/Lord_Howe"), // +10:30 (+11 DST — fails check then)
    (45900, "Pacific/Chatham"),     // +12:45 (+13:45 DST)
];

/// Map exact UTC offset seconds to a resolvable zone. Whole hours use
/// Etc/GMT±N (inverted sign), known half-hour offsets map to real zones,
/// anything else fails loudly instead of truncating.
fn tz_for_offset(off: i32) -> Option<Tz> {
    if off == 0 {
        return "UTC".parse().ok();
    }
    if let Some((_, iana)) = HALF_HOUR_ZONES.iter().find(|(s, _)| *s == off) {
        let tz: Tz = (*iana).parse().ok()?;
        // Only accept the zone while it actually holds this offset right now,
        // so card labels never contradict conversion math across DST shifts.
        if tz
            .offset_from_utc_datetime(&chrono::Utc::now().naive_utc())
            .fix()
            .local_minus_utc()
            == off
        {
            return Some(tz);
        }
        return None;
    }
    let hours = off / 3600;
    if hours * 3600 != off {
        return None; // unmatched fractional offset — surface error card
    }
    // Etc/GMT+5 means UTC-5
    let name = if hours > 0 {
        format!("Etc/GMT-{hours}")
    } else {
        format!("Etc/GMT+{}", -hours)
    };
    name.parse().ok()
}

/// Parse numeric offset tokens: `utc+05:30`, `gmt-8`, `utc_8` ('-' becomes '_'
/// in normalized keys). Returns seconds east of UTC, or None if not an offset
/// token. Range/validity of minutes enforced here; representability as a zone
/// is `tz_for_offset`'s job.
fn parse_utc_offset_token(t: &str) -> Option<i32> {
    let rest = t.strip_prefix("utc").or_else(|| t.strip_prefix("gmt"))?;
    let first = *rest.as_bytes().first()?;
    let (sign, digits) = match first {
        b'+' => (1i32, &rest[1..]),
        b'-' | b'_' => (-1i32, &rest[1..]),
        _ => return None,
    };
    // Colon form is strict: exactly two minute digits (`+1:2` rejected).
    let compact = match digits.split_once(':') {
        Some((h_part, m_part)) => {
            if h_part.is_empty()
                || m_part.len() != 2
                || !h_part.bytes().all(|c| c.is_ascii_digit())
                || !m_part.bytes().all(|c| c.is_ascii_digit())
            {
                return None;
            }
            format!("{h_part}{m_part}")
        }
        None => digits.chars().filter(|c| *c != '_').collect(),
    };
    if compact.is_empty() || compact.len() > 4 || !compact.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m) = match compact.len() {
        1 | 2 => (compact.parse::<i32>().ok()?, 0),
        3 => (
            compact[..1].parse::<i32>().ok()?,
            compact[1..].parse::<i32>().ok()?,
        ),
        _ => (
            compact[..2].parse::<i32>().ok()?,
            compact[2..].parse::<i32>().ok()?,
        ),
    };
    if h > 23 || m > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60))
}

/// Display label for an offset in seconds: `UTC+05:30`, `UTC-8`.
fn format_offset_label(off: i32) -> String {
    if off == 0 {
        return "UTC".into();
    }
    let sign = if off < 0 { "-" } else { "+" };
    let abs = off.abs();
    format!("UTC{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

pub(crate) fn local_as_tz() -> Option<(Tz, String)> {
    // Map local offset to a real resolvable zone (see tz_for_offset:
    // whole hours → Etc/GMT±N inverted, half-hours via curated table).
    let off = Local::now().offset().local_minus_utc(); // seconds
    Some((tz_for_offset(off)?, "LOCAL".into()))
}

#[cfg(test)]
mod timezone_query_tests {
    use super::{predict_tz, resolve_place, try_timezone};

    #[test]
    fn local_to_many_cities() {
        for q in [
            "15:00 here to london",
            "15:00 here to tokyo",
            "15:00 here to paris",
            "15:00 here to mumbai",
            "15:00 here to nyc",
            "15:00 here to dubai",
            "15:00 here to new york",
            "15:00 here to los angeles",
            "15:00 here to hong kong",
            "3pm here in tokyo",
        ] {
            assert!(try_timezone(q).is_some(), "expected hit for {q}");
        }
    }

    #[test]
    fn city_to_local_forms() {
        for q in [
            "15:00 london to here",
            "15:00 tokyo to here",
            "15:00 in london to here",
            "15:00 in tokyo to here",
            "15:00 in new york to here",
            "3pm tokyo to here",
            "3pm in paris to local",
        ] {
            assert!(try_timezone(q).is_some(), "expected hit for {q}");
        }
    }

    #[test]
    fn short_in_city_24h() {
        for q in [
            "15:00 in tokyo",
            "15:00 in mumbai",
            "15:00 in new york",
            "3pm in paris",
            "now in tokyo",
            "now in new york",
            "time in singapore",
        ] {
            assert!(try_timezone(q).is_some(), "expected hit for {q}");
        }
    }

    #[test]
    fn classic_zone_pairs() {
        for q in ["16:00 cet to ist", "4pm est to pst", "09:30 ist to utc"] {
            assert!(try_timezone(q).is_some(), "expected hit for {q}");
        }
    }

    #[test]
    fn resolve_multiword_and_compact() {
        assert!(resolve_place("new york").is_some());
        assert!(resolve_place("newyork").is_some());
        assert!(resolve_place("los angeles").is_some());
        assert!(resolve_place("hong kong").is_some());
        assert!(resolve_place("here").is_some());
        assert!(resolve_place("tokyo").is_some());
    }

    #[test]
    fn offset_shaped_garbage_never_resolves_as_utc() {
        // Invalid / sign-less offset forms must error out, not silently
        // fuzzy-match the "utc"/"gmt" aliases (silent-UTC regression class).
        for tok in ["utc+25", "gmt8", "utc8", "gmt_5x", "utc+1:2"] {
            assert!(predict_tz(tok).is_none(), "{tok} must not fuzzy-resolve");
            assert!(resolve_place(tok).is_none(), "{tok} must not resolve");
        }
    }
}

#[cfg(test)]
mod offset_token_tests {
    use super::{local_as_tz, parse_utc_offset_token, predict_tz, resolve_tz, tz_for_offset};
    use chrono::{Offset, TimeZone, Utc};

    #[test]
    fn colon_form_requires_two_minute_digits() {
        assert_eq!(parse_utc_offset_token("utc+1:2"), None);
        assert!(parse_utc_offset_token("utc+1:20").is_some());
    }

    #[test]
    fn marquesas_fixed_offset_maps() {
        // -9:30 fixed zone, no DST — deterministic.
        let off = -34200;
        let label = super::format_offset_label(off);
        assert_eq!(label, "UTC-09:30");
        if let Some(tz) = tz_for_offset(off) {
            assert_eq!(
                tz.offset_from_utc_datetime(&Utc::now().naive_utc())
                    .fix()
                    .local_minus_utc(),
                off
            );
        } else {
            panic!("Marquesas entry should resolve");
        }
    }

    fn offset_of(token: &str) -> Option<i32> {
        let (tz, label) = resolve_tz(token)?;
        assert!(!label.is_empty(), "empty label for {token}");
        Some(
            tz.offset_from_utc_datetime(&Utc::now().naive_utc())
                .fix()
                .local_minus_utc(),
        )
    }

    #[test]
    fn offset_parser_forms() {
        assert_eq!(parse_utc_offset_token("utc+05:30"), Some(19800));
        assert_eq!(parse_utc_offset_token("utc+0530"), Some(19800));
        assert_eq!(parse_utc_offset_token("gmt+3"), Some(10800));
        assert_eq!(parse_utc_offset_token("utc-8"), Some(-28800));
        // normalized key form ('-' → '_')
        assert_eq!(parse_utc_offset_token("utc_8"), Some(-28800));
        assert_eq!(parse_utc_offset_token("utc+9:36"), Some(34560));
        // non-tokens
        assert_eq!(parse_utc_offset_token("utc"), None);
        assert_eq!(parse_utc_offset_token("utopia"), None);
        assert_eq!(parse_utc_offset_token("utc+5:75"), None);
        assert_eq!(parse_utc_offset_token("london"), None);
    }

    #[test]
    fn offset_to_zone_half_hours() {
        let tz = tz_for_offset(19800).expect("+5:30 must map");
        assert_eq!(
            tz.offset_from_utc_datetime(&Utc::now().naive_utc())
                .fix()
                .local_minus_utc(),
            19800
        );
        let tz = tz_for_offset(20700).expect("+5:45 must map");
        assert_eq!(
            tz.offset_from_utc_datetime(&Utc::now().naive_utc())
                .fix()
                .local_minus_utc(),
            20700
        );
    }

    #[test]
    fn offset_to_zone_whole_hours() {
        let tz = tz_for_offset(-28800).expect("-8 must map");
        assert_eq!(
            tz.offset_from_utc_datetime(&Utc::now().naive_utc())
                .fix()
                .local_minus_utc(),
            -28800
        );
    }

    #[test]
    fn unmatched_fractional_offset_fails_loudly() {
        assert!(tz_for_offset(34620).is_none()); // +9:36
        assert!(resolve_tz("utc+9:36").is_none());
        // and never silently becomes UTC via fuzzy predict
        assert!(predict_tz("utc-8").is_none());
        assert!(predict_tz("gmt+3").is_none());
    }

    #[test]
    fn resolve_numeric_offsets() {
        assert_eq!(offset_of("utc-8"), Some(-28800));
        assert_eq!(offset_of("gmt+3"), Some(10800));
        assert_ne!(offset_of("gmt+3"), Some(0), "gmt+3 must not equal UTC");
        assert_eq!(offset_of("utc+5:30"), Some(19800));
        assert_eq!(offset_of("utc+0530"), Some(19800));
    }

    #[test]
    fn local_zone_matches_current_local_offset() {
        let (tz, label) = local_as_tz().expect("local zone resolvable");
        assert_eq!(label, "LOCAL");
        let expected = Utc::now()
            .with_timezone(&chrono::Local)
            .offset()
            .fix()
            .local_minus_utc();
        assert_eq!(
            tz.offset_from_utc_datetime(&Utc::now().naive_utc())
                .fix()
                .local_minus_utc(),
            expected
        );
    }
}
