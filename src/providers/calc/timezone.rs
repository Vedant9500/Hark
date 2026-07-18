use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use chrono::{Local, NaiveDateTime, NaiveTime, Timelike, Utc};
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
    static RE_NOW_TZ: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*(?:now|time)\s+(?:in|at)\s+(.+?)\s*$").unwrap()
    });
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
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?\s+(here|local)\s+(?:in|to|as|->|→)\s+(.+?)\s*$",
        )
        .unwrap()
    });
    // TIME in PLACE to here|local|PLACE — city → local (or city → city)
    // Also: TIME at PLACE to …
    static RE_IN_FROM_TO: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?\s+(?:in|at)\s+(.+?)\s+(?:to|as|->|→)\s+(.+?)\s*$",
        )
        .unwrap()
    });
    // TIME in|to PLACE — local → city (no "here" word). Requires am/pm OR :mm so bare
    // "15 tokyo" is not stolen.
    static RE_IN_CITY: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?\s*(am|pm)?\s+(?:in|to)\s+(.+?)\s*$",
        )
        .unwrap()
    });
    // Classic: TIME FROM to TO (optional am/pm; FROM/TO may be multi-word)
    static RE_TZ: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2}):(\d{2})(?::(\d{2}))?\s*(am|pm)?\s+(.+?)\s+(?:to|in|as|->|→)\s+(.+?)\s*$",
        )
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
                    hour, minute, second, from_tz, &from_label, to_tz, &to_label,
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
                hour, minute, second, from_tz, &from_label, to_tz, &to_label,
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
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+(here|local)\s+(?:in|to)\s+([a-zA-Z ]*?)\s*$",
        )
        .unwrap()
    });
    static RE2: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+(?:in|to)\s+([a-zA-Z ]*?)\s*$",
        )
        .unwrap()
    });
    static RE3: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+(?:in|at)\s+([a-zA-Z ]+?)\s+(?:to)\s+([a-zA-Z ]*?)\s*$",
        )
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
        let (from_tz, from_label) = resolve_place(from_prefix).or_else(|| predict_tz(from_prefix))?;
        let (to_tz, to_label) = if to_prefix.is_empty() {
            return None;
        } else if to_prefix.len() < 2 {
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
    let mut hits: Vec<(&str, &str, &str, i32)> = CITY_ALIASES
        .iter()
        .filter(|(alias, _, _)| {
            let a = *alias;
            a.starts_with(&p) || p.starts_with(a) || a.replace('_', "").starts_with(&p.replace('_', ""))
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
    let left_display = if from_label.eq_ignore_ascii_case("LOCAL")
        || from_label.eq_ignore_ascii_case("HERE")
    {
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
        "cet" => "Europe/Paris",       // CET/CEST with DST
        "cest" => "Europe/Paris",
        "eet" | "eest" => "Europe/Bucharest",
        "wet" | "west" => "Europe/Lisbon",
        "bst" => "Europe/London",      // British Summer — London handles GMT/BST
        "gmt+0" | "utc+0" => "UTC",
        "ist" => "Asia/Kolkata",       // India Standard Time
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
        "utc+5:30" | "utc+0530" => "Asia/Kolkata",
        "utc+1" | "utc+01" => "Europe/Paris",
        "utc-5" | "utc-05" => "America/New_York",
        "utc-8" | "utc-08" => "America/Los_Angeles",
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

pub(crate) fn local_as_tz() -> Option<(Tz, String)> {
    // Map local offset to Etc/GMT (note: Etc/GMT signs are inverted)
    let off = Local::now().offset().local_minus_utc(); // seconds
    let hours = off / 3600;
    // Etc/GMT+5 means UTC-5
    let name = if hours == 0 {
        "UTC".to_string()
    } else if hours > 0 {
        format!("Etc/GMT-{}", hours) // inverted
    } else {
        format!("Etc/GMT+{}", -hours)
    };
    let tz: Tz = name.parse().ok()?;
    Some((tz, "LOCAL".into()))
}



#[cfg(test)]
mod timezone_query_tests {
    use super::{resolve_place, try_timezone};

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
        for q in [
            "16:00 cet to ist",
            "4pm est to pst",
            "09:30 ist to utc",
        ] {
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
}
