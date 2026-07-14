use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use chrono::{Local, NaiveDateTime, NaiveTime, Timelike, Utc};
use chrono_tz::Tz;
use once_cell::sync::Lazy;
use regex::Regex;

/// `12pm here in london`, `16:00 cet to ist`, `4pm est to pst`, `now in tokyo`
pub(crate) fn try_timezone(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase().trim().to_string();

    // now in <tz> / time in <tz>
    static RE_NOW_TZ: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*(?:now|time)\s+(?:in|at)\s+([a-zA-Z_/+-]+)\s*$").unwrap()
    });
    if let Some(c) = RE_NOW_TZ.captures(&lower) {
        let (tz, label) = resolve_tz(c.get(1)?.as_str())?;
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

    // Natural: 12pm here in london | 12 pm here to london | 12:00 here in london
    static RE_HERE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+(here|local)\s+(?:in|to|as|->|→)\s+([a-zA-Z_/+-]+)\s*$",
        )
        .unwrap()
    });
    // 12pm in london (implies from local/here)
    static RE_IN_CITY: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)\s+(?:in|to)\s+([a-zA-Z_/+-]*)\s*$",
        )
        .unwrap()
    });
    // Classic: 16:00 cet to ist | 4:00 pm est -> pst
    static RE_TZ: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2}):(\d{2})(?::(\d{2}))?\s*(am|pm)?\s*([a-zA-Z_/+-]+)\s+(?:to|in|as|->|→)\s+([a-zA-Z_/+-]+)\s*$",
        )
        .unwrap()
    });
    // 4pm est to pst (no colon)
    static RE_TZ_COMPACT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)\s+([a-zA-Z_/+-]+)\s+(?:to|in|as|->|→)\s+([a-zA-Z_/+-]+)\s*$",
        )
        .unwrap()
    });

    if let Some(c) = RE_HERE.captures(&lower) {
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            None,
            c.get(3).map(|m| m.as_str()),
        )?;
        let (from_tz, from_label) = local_as_tz()?;
        let to_token = c.get(5)?.as_str();
        let (to_tz, to_label) = resolve_tz(to_token).or_else(|| predict_tz(to_token))?;
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }

    if let Some(c) = RE_IN_CITY.captures(&lower) {
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            None,
            c.get(3).map(|m| m.as_str()),
        )?;
        let (from_tz, from_label) = local_as_tz()?;
        let to_token = c.get(4)?.as_str();
        if to_token.is_empty() {
            return None;
        }
        let (to_tz, to_label) = resolve_tz(to_token).or_else(|| predict_tz(to_token))?;
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }

    if let Some(c) = RE_TZ_COMPACT.captures(&lower) {
        let (hour, minute, second) = parse_clock(
            c.get(1)?.as_str(),
            c.get(2).map(|m| m.as_str()),
            None,
            c.get(3).map(|m| m.as_str()),
        )?;
        let (from_tz, from_label) = resolve_tz(c.get(4)?.as_str())?;
        let to_token = c.get(5)?.as_str();
        let (to_tz, to_label) = resolve_tz(to_token).or_else(|| predict_tz(to_token))?;
        return build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label);
    }

    let caps = RE_TZ.captures(&lower)?;
    let (hour, minute, second) = parse_clock(
        caps.get(1)?.as_str(),
        Some(caps.get(2)?.as_str()),
        caps.get(3).map(|m| m.as_str()),
        caps.get(4).map(|m| m.as_str()),
    )?;
    let from_raw = caps.get(5)?.as_str();
    let to_raw = caps.get(6)?.as_str();
    let (from_tz, from_label) = resolve_tz(from_raw)?;
    let (to_tz, to_label) = resolve_tz(to_raw).or_else(|| predict_tz(to_raw))?;
    build_tz_conversion(hour, minute, second, from_tz, &from_label, to_tz, &to_label)
}

/// Prefix match for incomplete city/zone: `lon` → London
pub(crate) fn try_timezone_predict(q: &str) -> Option<SearchResult> {
    // Only when exact timezone parse failed
    let lower = q.to_lowercase();
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s+(here|local)\s+(?:in|to)\s+([a-zA-Z]*)\s*$",
        )
        .unwrap()
    });
    static RE2: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm)\s+(?:in|to)\s+([a-zA-Z]*)\s*$",
        )
        .unwrap()
    });
    if let Some(c) = RE.captures(&lower) {
        let prefix = c.get(5)?.as_str();
        if prefix.len() < 2 {
            return None;
        }
        if resolve_tz(prefix).is_some() {
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
    if let Some(c) = RE2.captures(&lower) {
        let prefix = c.get(4)?.as_str();
        if prefix.len() < 2 {
            return None;
        }
        if resolve_tz(prefix).is_some() {
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

pub(crate) fn predict_tz(prefix: &str) -> Option<(Tz, String)> {
    const CITIES: &[(&str, &str, &str)] = &[
        ("london", "Europe/London", "LONDON"),
        ("paris", "Europe/Paris", "PARIS"),
        ("berlin", "Europe/Berlin", "BERLIN"),
        ("tokyo", "Asia/Tokyo", "TOKYO"),
        ("mumbai", "Asia/Kolkata", "MUMBAI"),
        ("delhi", "Asia/Kolkata", "DELHI"),
        ("kolkata", "Asia/Kolkata", "KOLKATA"),
        ("india", "Asia/Kolkata", "IST"),
        ("newyork", "America/New_York", "NEW YORK"),
        ("nyc", "America/New_York", "NYC"),
        ("chicago", "America/Chicago", "CHICAGO"),
        ("denver", "America/Denver", "DENVER"),
        ("losangeles", "America/Los_Angeles", "LA"),
        ("la", "America/Los_Angeles", "LA"),
        ("sydney", "Australia/Sydney", "SYDNEY"),
        ("dubai", "Asia/Dubai", "DUBAI"),
        ("singapore", "Asia/Singapore", "SINGAPORE"),
        ("hongkong", "Asia/Hong_Kong", "HONG KONG"),
        ("moscow", "Europe/Moscow", "MOSCOW"),
        ("toronto", "America/Toronto", "TORONTO"),
        ("cet", "Europe/Paris", "CET"),
        ("ist", "Asia/Kolkata", "IST"),
        ("est", "America/New_York", "EST"),
        ("pst", "America/Los_Angeles", "PST"),
        ("gmt", "UTC", "GMT"),
        ("utc", "UTC", "UTC"),
    ];
    let p = prefix.to_lowercase().replace([' ', '_', '-'], "");
    if p.is_empty() {
        return None;
    }
    let mut hits: Vec<(&str, &str, &str, i32)> = CITIES
        .iter()
        .filter(|(alias, _, _)| alias.starts_with(&p) || p.starts_with(alias))
        .map(|(a, iana, label)| {
            let score = if *a == p {
                1000
            } else if a.starts_with(&p) {
                500 - a.len() as i32
            } else {
                100
            };
            (*a, *iana, *label, score)
        })
        .collect();
    hits.sort_by(|a, b| b.3.cmp(&a.3));
    let (_, iana, label, _) = hits.first()?;
    let tz: Tz = iana.parse().ok()?;
    Some((tz, (*label).to_string()))
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
    let t = token.trim().to_lowercase().replace(' ', "_");
    // Full IANA name
    if let Ok(tz) = t.parse::<Tz>() {
        return Some((tz, display_tz_label(&t)));
    }
    // Common aliases / abbreviations
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
