//! Quick wins (T2): base conversion output, roman numerals, BMI, steps→km,
//! random, uuid/password, and small text utilities.
//!
//! Queries: `255 to hex`, `ff to dec`, `1010 bin to dec`, `roman 1984`,
//! `bmi 180cm 75kg`, `10000 steps in km`, `dice`, `roll d20`, `coin`,
//! `random 6`, `random 1 10`, `uuid`, `password 16`, `wc hello world`,
//! `slug Hello World`, `case snake Hello World`.

use super::util::{card_result, format_number};
use crate::providers::SearchResult;
use once_cell::sync::Lazy;
use regex::Regex;

// ---------------------------------------------------------------------------
// Base conversion output
// ---------------------------------------------------------------------------

fn base_keyword(s: &str) -> Option<u32> {
    match s {
        "hex" | "hexadecimal" | "hexa" => Some(16),
        "bin" | "binary" => Some(2),
        "oct" | "octal" => Some(8),
        "dec" | "decimal" => Some(10),
        _ => None,
    }
}

/// Infer the source base of a bare literal: `0x`/`0b`/`0o` prefixes win;
/// otherwise all digits → decimal, letters a–f → hex.
fn infer_base(s: &str) -> Option<(u32, i128)> {
    let t = s.trim().to_ascii_lowercase();
    let (digits, base) = if let Some(h) = t.strip_prefix("0x") {
        (h, 16)
    } else if let Some(b) = t.strip_prefix("0b") {
        (b, 2)
    } else if let Some(o) = t.strip_prefix("0o") {
        (o, 8)
    } else if t.bytes().all(|b| b.is_ascii_digit()) {
        (t.as_str(), 10)
    } else if t.bytes().all(|b| b.is_ascii_hexdigit()) {
        (t.as_str(), 16)
    } else {
        return None;
    };
    let v = i128::from_str_radix(digits, base).ok()?;
    Some((base, v))
}

fn base_badge(s: &str) -> &'static str {
    match s {
        "hex" | "hexadecimal" | "hexa" => "hex",
        "bin" | "binary" => "bin",
        "oct" | "octal" => "oct",
        _ => "dec",
    }
}

fn base_card(v: i128, shown: &str, dst_base: u32, right_badge: &'static str) -> SearchResult {
    let dec = v.to_string();
    let hex = format!("{v:#x}");
    let bin = format!("{v:#b}");
    let oct = format!("{v:#o}");
    let out = match dst_base {
        16 => hex.trim_start_matches("0x").to_string(),
        2 => bin.trim_start_matches("0b").to_string(),
        8 => oct.trim_start_matches("0o").to_string(),
        _ => dec.clone(),
    };
    card_result(
        out.clone(),
        format!("{shown} → {out} · {hex} · {bin}"),
        format!("{shown} = {out}\n{hex}\n{oct}\n{bin}"),
        shown.to_string(),
        "base",
        out,
        right_badge,
    )
}

fn try_base_convert(q: &str) -> Option<SearchResult> {
    let lower = q.trim().to_ascii_lowercase();
    // `255 to hex`, `0x1f to dec`, `ff to dec`
    static RE_DIRECT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^([0-9a-f]+|0x[0-9a-f]+|0b[01]+|0o[0-7]+)\s+(?:to|in|as)\s+(hex(?:adecimal)?|bin(?:ary)?|oct(?:al)?|dec(?:imal)?)\s*$",
        )
        .unwrap()
    });
    // `1010 bin to dec` — explicit source base.
    static RE_SRC_BASE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^([0-9a-f]+)\s+(hex(?:a(?:decimal)?)?|bin(?:ary)?|oct(?:al)?|dec(?:imal)?)\s+(?:to|in|as)\s+(hex(?:a(?:decimal)?)?|bin(?:ary)?|oct(?:al)?|dec(?:imal)?)\s*$")
            .unwrap()
    });

    if let Some(c) = RE_SRC_BASE.captures(&lower) {
        let digits = c.get(1)?.as_str();
        let src_base = base_keyword(c.get(2)?.as_str())?;
        let dst_base = base_keyword(c.get(3)?.as_str())?;
        if src_base == dst_base {
            return None;
        }
        let v = i128::from_str_radix(digits, src_base).ok()?;
        return Some(base_card(
            v,
            q.trim(),
            dst_base,
            base_badge(c.get(3)?.as_str()),
        ));
    }

    if let Some(c) = RE_DIRECT.captures(&lower) {
        let dst_base = base_keyword(c.get(2)?.as_str())?;
        let (src_base, v) = infer_base(c.get(1)?.as_str())?;
        if src_base == dst_base {
            return None;
        }
        return Some(base_card(
            v,
            q.trim(),
            dst_base,
            base_badge(c.get(2)?.as_str()),
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// Roman numerals
// ---------------------------------------------------------------------------

fn to_roman(mut n: u64) -> Option<String> {
    if !(1..=3999).contains(&n) {
        return None;
    }
    const M: [(u64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (v, s) in M {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    Some(out)
}

fn from_roman(s: &str) -> Option<u64> {
    let chars: Vec<char> = s.to_ascii_uppercase().chars().collect();
    let mut total = 0u64;
    let mut prev = 0u64;
    for &c in chars.iter().rev() {
        let v = match c {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => return None,
        };
        if v < prev {
            total = total.checked_sub(v)?;
        } else {
            total += v;
            prev = v;
        }
    }
    if total == 0 {
        None
    } else {
        Some(total)
    }
}

fn try_roman(q: &str) -> Option<SearchResult> {
    let lower = q.trim().to_ascii_lowercase();
    static RE_FWD: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^roman\s+(\d{1,4})\s*$").unwrap());
    static RE_REV: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^roman\s+([ivxlcdm]+)\s*$").unwrap());
    let shown = q.trim().to_string();

    if let Some(c) = RE_FWD.captures(&lower) {
        let n: u64 = c.get(1)?.as_str().parse().ok()?;
        let r = to_roman(n)?;
        return Some(card_result(
            r.clone(),
            format!("{n} → {r}"),
            r.clone(),
            shown,
            "roman",
            r,
            "numeral",
        ));
    }
    if let Some(c) = RE_REV.captures(&lower) {
        let r = c.get(1)?.as_str();
        let n = from_roman(r)?;
        return Some(card_result(
            n.to_string(),
            format!("{r} → {n}"),
            n.to_string(),
            shown,
            "roman",
            n.to_string(),
            "number",
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// BMI
// ---------------------------------------------------------------------------

/// Inches only go 0–11. An over-range value like `5'55"` is a dropped decimal
/// (5'5.5"), not 5 ft 55 in — shift the decimal point left until it fits.
fn normalize_inches(mut v: f64) -> f64 {
    let mut shifts = 0;
    while v >= 12.0 && shifts < 4 {
        v /= 10.0;
        shifts += 1;
    }
    v
}

/// Parse a height spec to meters. Accepts metric (`180cm`, `1.8m`) and
/// imperial feet+inches (`5ft`, `5 feet 5 inches`, `5'5"`, `5' 5"`, `5'`,
/// `5ft 5in`, `5'55"`). A bare prime `'` means feet, `"` means inches.
fn parse_height(s: &str) -> Option<f64> {
    static RE_FTIN: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r#"(?i)^\s*(\d+(?:\.\d+)?)\s*(?:'|ft|feet|foot)\s*(\d+(?:\.\d+)?)?\s*(?:"|in|inch|inches)?\s*$"#,
        )
        .unwrap()
    });
    if let Some(c) = RE_FTIN.captures(s.trim()) {
        let ft: f64 = c.get(1)?.as_str().parse().ok()?;
        let inches: f64 = match c.get(2) {
            Some(m) => normalize_inches(m.as_str().parse().ok()?),
            None => 0.0,
        };
        return Some(ft * 0.3048 + inches * 0.0254);
    }
    static RE_MET: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*(\d+(?:\.\d+)?)\s*(cm|m|meters?|metres?|in|inches?)\s*$").unwrap()
    });
    if let Some(c) = RE_MET.captures(s.trim()) {
        let v: f64 = c.get(1)?.as_str().parse().ok()?;
        return Some(match c.get(2)?.as_str() {
            "cm" => v / 100.0,
            "in" | "inch" | "inches" => v * 0.0254,
            _ => v,
        });
    }
    None
}

fn try_bmi(q: &str) -> Option<SearchResult> {
    static RE_BMI: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^bmi\s+(.+?)\s+(\d+(?:\.\d+)?)\s*(kg|kilograms?|kgs|lb|lbs|pounds?)?\s*$")
            .unwrap()
    });
    let c = RE_BMI.captures(q)?;
    let h_m = parse_height(c.get(1)?.as_str())?;
    let wv: f64 = c.get(2)?.as_str().parse().ok()?;
    let kg = match c.get(3).map(|m| m.as_str()).unwrap_or("kg") {
        "kg" | "kilogram" | "kilograms" | "kgs" | "" => wv,
        "lb" | "lbs" | "pound" | "pounds" => wv * 0.453_592_37,
        _ => return None,
    };
    if h_m <= 0.0 || kg <= 0.0 {
        return None;
    }
    let bmi = kg / (h_m * h_m);
    let cat = if bmi < 18.5 {
        "underweight"
    } else if bmi < 25.0 {
        "normal"
    } else if bmi < 30.0 {
        "overweight"
    } else {
        "obese"
    };
    let shown = q.trim().to_string();
    let title = {
        let s = format!("{bmi:.1}");
        s.trim_end_matches(".0").to_string()
    };
    Some(card_result(
        title.clone(),
        format!(
            "{} cm · {} kg · {}",
            format_number(h_m * 100.0),
            format_number(kg),
            cat
        ),
        format!("BMI {} kg/m² → {}", title, cat),
        shown,
        "bmi",
        title,
        cat,
    ))
}

// ---------------------------------------------------------------------------
// Height conversion
// ---------------------------------------------------------------------------

/// Height conversion for forms the generic length converter cannot parse:
/// `5'5" to cm`, `5ft 5in in cm`, `5ft to m`, `5'11" to cm`.
fn try_height(q: &str) -> Option<SearchResult> {
    static RE_H: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(.+?)\s+(?:to|in|as)\s+(cm|centimeters?|centimetres?|m|meters?|metres?|ft|feet|foot|in|inch|inches)\s*$",
        )
        .unwrap()
    });
    let c = RE_H.captures(q)?;
    let m = parse_height(c.get(1)?.as_str())?;
    if m <= 0.0 {
        return None;
    }
    let target = c.get(2)?.as_str();
    let total_in = m / 0.0254;
    let ft = (total_in / 12.0).floor();
    let inch = total_in - ft * 12.0;
    let (title, right_badge) = match target {
        "cm" | "centimeter" | "centimeters" | "centimetre" | "centimetres" => {
            (format!("{} cm", format_number(m * 100.0)), "cm")
        }
        "m" | "meter" | "meters" | "metre" | "metres" => (format!("{} m", format_number(m)), "m"),
        "ft" | "feet" | "foot" => {
            let r_in = inch.round();
            let (f2, i2) = if r_in >= 12.0 {
                (ft + 1.0, 0.0)
            } else {
                (ft, r_in)
            };
            if i2 == 0.0 {
                (format!("{} ft", format_number(f2)), "ft")
            } else {
                (
                    format!("{} ft {} in", format_number(f2), format_number(i2)),
                    "ft",
                )
            }
        }
        "in" | "inch" | "inches" => (format!("{} in", format_number(total_in)), "in"),
        _ => return None,
    };
    Some(card_result(
        title.clone(),
        format!("{} → {}", c.get(1)?.as_str(), title),
        format!("{} m", format_number(m)),
        q.trim().to_string(),
        "height",
        title,
        right_badge,
    ))
}

// ---------------------------------------------------------------------------
// Steps → distance
// ---------------------------------------------------------------------------

/// Average adult stride ≈ 0.762 m (2.5 ft). Assumption; stated in the card.
const STEP_STRIDE_M: f64 = 0.762;

fn try_steps(q: &str) -> Option<SearchResult> {
    static RE_DIST: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(\d+)\s+steps?\s+(?:in|to|as)\s+(km|kilometers?|kilometres?|mi|miles?|m|meters?|metres?)\s*$",
        )
        .unwrap()
    });
    static RE_REV: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(\d+(?:\.\d+)?)\s*(km|kilometers?|kilometres?|mi|miles?|m|meters?|metres?)\s+(?:in|to|as)\s+steps?\s*$",
        )
        .unwrap()
    });
    let shown = q.trim().to_string();

    if let Some(c) = RE_DIST.captures(q) {
        let n: f64 = c.get(1)?.as_str().parse().ok()?;
        let meters = n * STEP_STRIDE_M;
        let (out, label) = match c.get(2)?.as_str() {
            "km" | "kilometer" | "kilometers" | "kilometre" | "kilometres" => {
                (meters / 1000.0, "km")
            }
            "mi" | "mile" | "miles" => (meters / 1609.344, "mi"),
            _ => (meters, "m"),
        };
        let title = format!("{} {}", format_number(out), label);
        let copy = format!(
            "{n} steps × {} m stride = {}",
            format_number(STEP_STRIDE_M),
            title
        );
        return Some(card_result(
            title.clone(),
            format!(
                "{} steps × {} m",
                format_number(n),
                format_number(STEP_STRIDE_M)
            ),
            copy,
            shown,
            "steps",
            title,
            label,
        ));
    }

    if let Some(c) = RE_REV.captures(q) {
        let v: f64 = c.get(1)?.as_str().parse().ok()?;
        let meters = match c.get(2)?.as_str() {
            "km" | "kilometer" | "kilometers" | "kilometre" | "kilometres" => v * 1000.0,
            "mi" | "mile" | "miles" => v * 1609.344,
            _ => v,
        };
        let steps = (meters / STEP_STRIDE_M).round();
        let title = format!("{} steps", format_number(steps));
        return Some(card_result(
            title.clone(),
            format!(
                "{} ÷ {} m stride",
                format_number(meters),
                format_number(STEP_STRIDE_M)
            ),
            format!("{} m = {}", format_number(meters), title),
            shown,
            "steps",
            title,
            "steps",
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// Download speed / transfer ETA
// ---------------------------------------------------------------------------

/// Split `55GB`, `5.5 GB`, `150mbps`, `150MB/s` → (number, unit-token).
/// Spaces removed; `/s` or `/sec` stays part of the speed unit token.
fn split_num_unit(s: &str) -> Option<(f64, String)> {
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^(\d+(?:\.\d+)?)([a-zA-Z]+(?:/[a-zA-Z]+)?)$").unwrap());
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let c = RE.captures(&t)?;
    Some((
        c.get(1)?.as_str().parse().ok()?,
        c.get(2)?.as_str().to_string(),
    ))
}

/// Size token → bytes. Lowercase `kb`/`mb`/`gb` are treated as *bytes* (sizes
/// are almost never meant in bits); explicit `bit(s)`/`kbit`/`megabit`…
/// give bits. `KiB`/`MiB`/`GiB` are binary (×1024).
fn parse_size_bytes(s: &str) -> Option<f64> {
    let (v, u) = split_num_unit(s)?;
    let mult = match u.to_ascii_lowercase().as_str() {
        "b" | "byte" | "bytes" => 1.0,
        "kb" | "kilobyte" | "kilobytes" => 1e3,
        "mb" | "megabyte" | "megabytes" => 1e6,
        "gb" | "gigabyte" | "gigabytes" => 1e9,
        "tb" | "terabyte" | "terabytes" => 1e12,
        "pb" => 1e15,
        "kib" => 1024.0,
        "mib" => 1024f64 * 1024.0,
        "gib" => 1024f64 * 1024.0 * 1024.0,
        "tib" => 1024f64 * 1024.0 * 1024.0 * 1024.0,
        "bit" | "bits" => 0.125,
        "kbit" | "kilobit" | "kilobits" => 125.0,
        "mbit" | "megabit" | "megabits" => 125_000.0,
        "gbit" | "gigabit" | "gigabits" => 1.25e8,
        "tbit" | "terabit" | "terabits" => 1.25e11,
        _ => return None,
    };
    Some(v * mult)
}

/// Speed token → bytes per second. Uppercase `B` (`MB/s`, `MBps`, `MB`) means
/// bytes; lowercase `b` (`mbps`, `mb/s`, `Mb`) means bits (÷8). Byte-ness is
/// matched structurally on the case-preserved token (the magnitude table below
/// folds case): trailing capital `B` (`KB`, `KiB`), capital `B` before
/// `p`/`P`/`/` (`Bps`, `MB/S`), or an explicit byte word in any case.
fn parse_speed_bps(s: &str) -> Option<f64> {
    let (v, u) = split_num_unit(s)?;
    let is_byte = u.ends_with('B')
        || u.chars()
            .zip(u.chars().skip(1))
            .any(|(c, n)| c == 'B' && matches!(n, 'p' | 'P' | '/'))
        || u.to_ascii_lowercase().contains("byte");
    let mag = match u.to_ascii_lowercase().as_str() {
        "b" | "bps" | "b/s" | "b/sec" | "bit" | "bits" | "bit/s" | "bits/s" => 1.0,
        "kb" | "kbps" | "kb/s" | "kbit" | "kbits" | "kilobit" | "kilobits" | "kilobit/s"
        | "kilobits/s" => 1e3,
        "mb" | "mbps" | "mb/s" | "mbit" | "mbits" | "megabit" | "megabits" | "megabit/s"
        | "megabits/s" => 1e6,
        "gb" | "gbps" | "gb/s" | "gbit" | "gbits" | "gigabit" | "gigabits" | "gigabit/s"
        | "gigabits/s" => 1e9,
        "tb" | "tbps" | "tb/s" | "tbit" | "tbits" | "terabit" | "terabits" | "terabit/s"
        | "terabits/s" => 1e12,
        "kib" | "kib/s" | "kibps" => 1024.0,
        "mib" | "mib/s" | "mibps" => 1024f64 * 1024.0,
        "gib" | "gib/s" | "gibps" => 1024f64 * 1024.0 * 1024.0,
        "tib" | "tib/s" | "tibps" => 1024f64 * 1024.0 * 1024.0 * 1024.0,
        "byte" | "bytes" | "bytes/s" | "byte/s" => 1.0,
        "kilobyte" | "kilobytes" | "kilobyte/s" | "kilobytes/s" | "kbyte" | "kbytes" => 1e3,
        "megabyte" | "megabytes" | "megabyte/s" | "megabytes/s" | "mbyte" | "mbytes" => 1e6,
        "gigabyte" | "gigabytes" | "gigabyte/s" | "gigabytes/s" | "gbyte" | "gbytes" => 1e9,
        "terabyte" | "terabytes" | "terabyte/s" | "terabytes/s" | "tbyte" | "tbytes" => 1e12,
        _ => return None,
    };
    Some(v * mag * if is_byte { 1.0 } else { 0.125 })
}

/// Human duration from seconds: `45s`, `5m 03s`, `1h 07m`, `2d 3h`.
fn fmt_duration(secs: f64) -> String {
    let mut t = secs.round() as i64;
    if t < 1 {
        return "less than a second".into();
    }
    let d = t / 86_400;
    t %= 86_400;
    let h = t / 3_600;
    t %= 3_600;
    let m = t / 60;
    let s = t % 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Human speed from bytes/s: `150 MB/s ≈ 1.2 Gbps`.
fn fmt_speed_bps(bps: f64) -> String {
    let bits = bps * 8.0;
    if bits >= 1e9 {
        format!("{} Gbps", format_number(bits / 1e9))
    } else if bits >= 1e6 {
        format!("{} Mbps", format_number(bits / 1e6))
    } else if bits >= 1e3 {
        format!("{} kbps", format_number(bits / 1e3))
    } else {
        format!("{} bps", format_number(bits))
    }
}

/// Human size from bytes: `55,000,000,000 B` → `55 GB`.
fn fmt_size_bytes(bytes: f64) -> String {
    if bytes >= 1e12 {
        format!("{} TB", format_number(bytes / 1e12))
    } else if bytes >= 1e9 {
        format!("{} GB", format_number(bytes / 1e9))
    } else if bytes >= 1e6 {
        format!("{} MB", format_number(bytes / 1e6))
    } else if bytes >= 1e3 {
        format!("{} KB", format_number(bytes / 1e3))
    } else {
        format!("{} B", format_number(bytes))
    }
}

/// Transfer ETA: `55GB at 150MB/s`, `55GB @ 150MBps`, `1.5gb at 100mbps`,
/// `150MB/s for 55GB`, `how long to download 55GB at 150MB/s`.
fn try_speed(q: &str) -> Option<SearchResult> {
    static RE_SPEED: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(?:(?:how\s+long(?:\s+does\s+it\s+take)?\s+to|time\s+to)\s+)?(?:download|transfer|dl)?\s*(.+?)\s+(?:at|@|using|for)\s+(.+?)\s*$",
        )
        .unwrap()
    });
    let c = RE_SPEED.captures(q)?;
    let a = c.get(1)?.as_str();
    let b = c.get(2)?.as_str();
    // `for` puts speed first (`150MB/s for 55GB`); try both orders so
    // `55GB for 150MB/s` works too.
    let (size_token, speed_token, bytes, speed) = match (parse_size_bytes(a), parse_speed_bps(b)) {
        (Some(sz), Some(sp)) => (a.to_string(), b.to_string(), sz, sp),
        _ => match (parse_speed_bps(a), parse_size_bytes(b)) {
            (Some(sp), Some(sz)) => (b.to_string(), a.to_string(), sz, sp),
            _ => return None,
        },
    };
    if bytes <= 0.0 || speed <= 0.0 {
        return None;
    }
    let secs = bytes / speed;
    let dur = fmt_duration(secs);
    Some(card_result(
        dur.clone(),
        format!(
            "{} ÷ {} · {} ≈ {}",
            size_token,
            speed_token,
            speed_token,
            fmt_speed_bps(speed)
        ),
        format!(
            "{size_token} at {speed_token} → {dur} · {}",
            fmt_size_bytes(bytes)
        ),
        size_token,
        "download",
        dur,
        "eta",
    ))
}

// ---------------------------------------------------------------------------
// Random / dice / coin (own tiny PRNG — no new deps)
// ---------------------------------------------------------------------------

use std::cell::Cell;

thread_local! {
    static RNG: Cell<u64> = Cell::new(initial_seed());
}

/// Best-effort read from the OS CSPRNG (/dev/urandom); None on any failure.
fn read_urandom(n: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Credential-grade random bytes from the OS CSPRNG. None on any failure —
/// callers must refuse to produce output rather than fall back to the
/// predictable xorshift stream (CWE-338).
fn csprng_bytes(n: usize) -> Option<Vec<u8>> {
    read_urandom(n)
}

fn initial_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut s = nanos ^ (std::process::id() as u64).wrapping_mul(0x9e37_79b9);
    // Mix OS entropy into the seed when available.
    if let Some(b) = read_urandom(8) {
        let mut e = [0u8; 8];
        e.copy_from_slice(&b);
        s ^= u64::from_le_bytes(e);
    }
    s
}

fn next_u64() -> u64 {
    RNG.with(|r| {
        let mut x = r.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        r.set(x);
        x
    })
}

/// Uniform in [0, 1).
fn rng_f64() -> f64 {
    (next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// Uniform integer in [lo, hi] inclusive (overflow/bias-safe).
fn rng_int(lo: i64, hi: i64) -> i64 {
    if hi <= lo {
        return lo;
    }
    let span = (hi as i128 - lo as i128 + 1) as u128; // ≤ 2^64, fits
                                                      // Rejection-sample a u128 against span to avoid modulo bias.
    loop {
        let v = ((next_u64() as u128) << 64) | (next_u64() as u128);
        let limit = u128::MAX - (u128::MAX % span);
        if v < limit {
            // i128 accumulation: (v % span) as i64 alone would wrap ≥ 2^63.
            return (lo as i128 + (v % span) as i128) as i64;
        }
    }
}

fn try_random(q: &str) -> Option<SearchResult> {
    let lower = q.trim().to_ascii_lowercase();
    static RE_ROLL: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^roll\s+d(\d{1,6})\s*$").unwrap());
    static RE_RAND_N: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^random\s+(\d{1,10})\s*$").unwrap());
    static RE_RAND_AB: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^random\s+(-?\d+)\s+(-?\d+)\s*$").unwrap());

    if lower == "dice" {
        let v = rng_int(1, 6);
        let s = v.to_string();
        return Some(card_result(
            s.clone(),
            "d6".into(),
            s.clone(),
            "dice".into(),
            "dice",
            s.clone(),
            "d6",
        ));
    }
    if let Some(c) = RE_ROLL.captures(&lower) {
        let n: i64 = c.get(1)?.as_str().parse().ok()?;
        if !(1..=1_000_000).contains(&n) {
            return None;
        }
        let v = rng_int(1, n);
        let s = v.to_string();
        return Some(card_result(
            s.clone(),
            format!("d{n}"),
            s.clone(),
            "roll".into(),
            "dice",
            s,
            "result",
        ));
    }
    if lower == "coin" {
        let heads = rng_int(0, 1) == 0;
        return Some(card_result(
            if heads { "heads" } else { "tails" }.into(),
            "coin flip".into(),
            if heads { "heads" } else { "tails" }.into(),
            "coin".into(),
            "coin",
            if heads { "heads" } else { "tails" }.into(),
            "result",
        ));
    }
    if lower == "random" {
        let s = format_number(rng_f64());
        return Some(card_result(
            s.clone(),
            "uniform [0,1)".into(),
            s.clone(),
            "random".into(),
            "random",
            s.clone(),
            "result",
        ));
    }
    if let Some(c) = RE_RAND_AB.captures(&lower) {
        let a: i64 = c.get(1)?.as_str().parse().ok()?;
        let b: i64 = c.get(2)?.as_str().parse().ok()?;
        let v = rng_int(a, b);
        let s = v.to_string();
        return Some(card_result(
            s.clone(),
            format!("{a}..={b}"),
            s.clone(),
            "random".into(),
            "random",
            s.clone(),
            "result",
        ));
    }
    if let Some(c) = RE_RAND_N.captures(&lower) {
        let n: i64 = c.get(1)?.as_str().parse().ok()?;
        if n <= 0 {
            return None;
        }
        let v = rng_int(1, n);
        let s = v.to_string();
        return Some(card_result(
            s.clone(),
            format!("1..={n}"),
            s.clone(),
            "random".into(),
            "random",
            s.clone(),
            "result",
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// UUID v4 + password
// ---------------------------------------------------------------------------

fn uuid_v4(bytes: Vec<u8>) -> String {
    let mut b = [0u8; 16];
    b.copy_from_slice(&bytes);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

fn try_uuid(q: &str) -> Option<SearchResult> {
    let lower = q.trim().to_ascii_lowercase();
    if lower != "uuid" {
        return None;
    }
    let s = uuid_v4(csprng_bytes(16)?);
    Some(card_result(
        s.clone(),
        "UUID v4".into(),
        s.clone(),
        "uuid".into(),
        "uuid",
        s,
        "v4",
    ))
}

const PASSWORD_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn try_password(q: &str) -> Option<SearchResult> {
    static RE_PW: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^password(?:\s+(\d{1,3}))?\s*$").unwrap());
    let lower = q.trim().to_ascii_lowercase();
    let c = RE_PW.captures(&lower)?;
    let mut len = c
        .get(1)
        .and_then(|m| m.as_str().parse::<usize>().ok())
        .unwrap_or(16);
    len = len.clamp(4, 128);
    // Passwords are credentials: only OS entropy is acceptable. If the
    // CSPRNG is unavailable, fail (None → "no result") rather than emit a
    // predictable value presented as a password (CWE-338).
    let entropy = || csprng_bytes(len.max(64));
    let mut pool = entropy()?;
    let mut pool_i = 0;
    // Rejection sampling over bytes keeps draws uniform across the 62 chars.
    let mut s = String::with_capacity(len);
    while s.len() < len {
        if pool_i >= pool.len() {
            pool = entropy()?;
            pool_i = 0;
        }
        let b = pool[pool_i];
        pool_i += 1;
        if b < 248 {
            // 62 * 4 = 248 accepted values → b % 62 is unbiased.
            s.push(PASSWORD_CHARS[(b % 62) as usize] as char);
        }
    }
    Some(card_result(
        s.clone(),
        format!("{len}-char password"),
        s.clone(),
        "password".into(),
        "password",
        s,
        "generated",
    ))
}

// ---------------------------------------------------------------------------
// Text utils: word count, slug, case conversion
// ---------------------------------------------------------------------------

fn try_text(q: &str) -> Option<SearchResult> {
    let lower = q.trim().to_ascii_lowercase();
    static RE_WC: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^wc\s+(.+)$").unwrap());
    static RE_SLUG: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^slug\s+(.+)$").unwrap());
    static RE_CASE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^case\s+(snake|kebab|screaming|pascal|camel|upper|lower|title)\s+(.+)$")
            .unwrap()
    });
    let shown = q.trim().to_string();

    if let Some(c) = RE_WC.captures(&lower) {
        let text = c.get(1)?.as_str();
        let words = text.split_whitespace().count();
        let chars = text.chars().count();
        let title = format!("{words} words");
        return Some(card_result(
            title.clone(),
            format!("{chars} characters"),
            format!("{words} words · {chars} characters"),
            shown,
            "words",
            title,
            "count",
        ));
    }

    if let Some(c) = RE_SLUG.captures(&lower) {
        let text = c.get(1)?.as_str();
        let slug: String = text
            .to_ascii_lowercase()
            .chars()
            .map(|ch| {
                if ch.is_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        if slug.is_empty() {
            return None;
        }
        return Some(card_result(
            slug.clone(),
            format!("slug: {text}"),
            slug.clone(),
            shown,
            "slug",
            slug,
            "result",
        ));
    }

    if let Some(c) = RE_CASE.captures(&lower) {
        let style = c.get(1)?.as_str();
        let text = c.get(2)?.as_str();
        let words: Vec<String> = text
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect();
        if words.is_empty() {
            return None;
        }
        let cap = |w: &str| -> String {
            let mut cs = w.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        };
        let out: String = match style {
            "snake" => words.join("_"),
            "kebab" => words.join("-"),
            "screaming" => words.join("_").to_uppercase(),
            "pascal" => words.iter().map(|w| cap(w)).collect(),
            "camel" => {
                let mut it = words.iter();
                let first = it.next()?.to_ascii_lowercase();
                first + &it.map(|w| cap(w)).collect::<String>()
            }
            "upper" => words.join(" ").to_uppercase(),
            "lower" => words.join(" ").to_lowercase(),
            "title" => words
                .iter()
                .map(|w| cap(&w.to_ascii_lowercase()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => return None,
        };
        return Some(card_result(
            out.clone(),
            format!("case {style}: {text}"),
            out.clone(),
            shown,
            "case",
            out,
            "case",
        ));
    }
    None
}

// ---------------------------------------------------------------------------

pub(crate) fn try_quickwin(q: &str) -> Option<SearchResult> {
    if let Some(r) = try_base_convert(q) {
        return Some(r);
    }
    if let Some(r) = try_roman(q) {
        return Some(r);
    }
    if let Some(r) = try_height(q) {
        return Some(r);
    }
    if let Some(r) = try_bmi(q) {
        return Some(r);
    }
    if let Some(r) = try_steps(q) {
        return Some(r);
    }
    if let Some(r) = try_speed(q) {
        return Some(r);
    }
    if let Some(r) = try_random(q) {
        return Some(r);
    }
    if let Some(r) = try_uuid(q) {
        return Some(r);
    }
    if let Some(r) = try_password(q) {
        return Some(r);
    }
    if let Some(r) = try_text(q) {
        return Some(r);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_conversion() {
        let r = try_quickwin("255 to hex").expect("hex");
        assert_eq!(r.title, "ff");
        assert_eq!(r.conversion.as_ref().unwrap().right_badge, "hex");
        let r = try_quickwin("ff to dec").expect("dec");
        assert_eq!(r.title, "255");
        let r = try_quickwin("1010 to bin").expect("bin");
        assert_eq!(r.title, "1111110010");
        let r = try_quickwin("1010 bin to dec").expect("explicit src");
        assert_eq!(r.title, "10");
        let r = try_quickwin("0x1f to dec").expect("0x");
        assert_eq!(r.title, "31");
        // `hexa` keyword is reachable and >u64 literals convert (was silent None).
        let r = try_quickwin("ff hexa to dec").expect("hexa keyword");
        assert_eq!(r.title, "255");
        let big = "f".repeat(20);
        let r = try_quickwin(format!("{big} hex to dec").as_str()).expect(">u64 hex");
        assert_eq!(r.title, i128::from_str_radix(&big, 16).unwrap().to_string());
        let r = try_quickwin("255 to oct").expect("oct");
        assert_eq!(r.title, "377");
        // Must not steal conversions / financial.
        assert!(try_quickwin("5 km to miles").is_none());
        assert!(try_quickwin("100 to 150").is_none());
        assert!(try_quickwin("100 usd to eur").is_none());
    }

    #[test]
    fn roman() {
        let r = try_quickwin("roman 1984").expect("roman");
        assert_eq!(r.title, "MCMLXXXIV");
        let r = try_quickwin("roman mcmxciv").expect("reverse");
        assert_eq!(r.title, "1994");
        assert!(try_quickwin("roman 0").is_none());
        assert!(try_quickwin("roman 5000").is_none());
    }

    #[test]
    fn bmi() {
        let r = try_quickwin("bmi 180cm 75kg").expect("bmi");
        assert_eq!(r.title, "23.1");
        assert_eq!(r.conversion.as_ref().unwrap().right_badge, "normal");
        let r = try_quickwin("bmi 1.8m 75kg").expect("m height");
        assert_eq!(r.title, "23.1");
        let r = try_quickwin("bmi 70 in 165 lb").expect("imperial");
        assert_eq!(r.title, "23.7");
        // feet + inches heights
        let r = try_quickwin("bmi 5ft 5inch 55kg").expect("ft in");
        assert_eq!(r.title, "20.2");
        assert_eq!(r.conversion.as_ref().unwrap().right_badge, "normal");
        let r = try_quickwin("bmi 5'5\" 55kg").expect("prime");
        assert_eq!(r.title, "20.2");
        let r = try_quickwin("bmi 5' 11\" 80kg").expect("prime spaced");
        assert_eq!(r.title, "24.6");
        let r = try_quickwin("bmi 5ft 6in 150 lb").expect("mixed");
        assert_eq!(r.title, "24.2");
        // Over-range inches: 5'55" = 5'5.5".
        let r = try_quickwin("bmi 5'55\" 55kg").expect("over-range");
        assert_eq!(r.title, "19.9");
    }

    #[test]
    fn height() {
        let r = try_quickwin("5'5\" to cm").expect("prime to cm");
        assert_eq!(r.title, "165.1 cm");
        let r = try_quickwin("5ft 5inch to cm").expect("words to cm");
        assert_eq!(r.title, "165.1 cm");
        let r = try_quickwin("5'11\" to cm").expect("prime spaced");
        assert_eq!(r.title, "180.34 cm");
        let r = try_quickwin("5ft to m").expect("ft to m");
        assert_eq!(r.title, "1.524 m");
        let r = try_quickwin("5'5\" in inches").expect("to inches");
        assert_eq!(r.title, "65 in");
        let r = try_quickwin("180cm to ft").expect("cm to ft");
        assert_eq!(r.title, "5 ft 11 in");
        // Over-range inches are a dropped decimal, not 5 ft 55 in.
        let r = try_quickwin("5'55\" to cm").expect("over-range");
        assert_eq!(r.title, "166.37 cm");
        let r = try_quickwin("5'55\" to ft").expect("over-range ft");
        assert_eq!(r.title, "5 ft 6 in");
        let r = try_quickwin("6'15\" to cm").expect("over-range 15");
        assert_eq!(r.title, "186.69 cm");
    }

    #[test]
    fn steps() {
        let r = try_quickwin("10000 steps in km").expect("steps");
        assert_eq!(r.title, "7.62 km");
        assert_eq!(r.conversion.as_ref().unwrap().left_badge, "steps");
        let r = try_quickwin("10000 steps to miles").expect("miles");
        assert_eq!(r.title, "4.7348 mi");
        let r = try_quickwin("7.62 km in steps").expect("reverse");
        assert_eq!(r.title, "10000 steps");
        let r = try_quickwin("10 km to steps").expect("km to steps");
        assert_eq!(r.title, "13123 steps");
    }

    #[test]
    fn download_speed() {
        // 55 GB (55e9 B) at 150 MB/s (150e6 B/s) → 366.7 s ≈ 6m 07s
        let r = try_quickwin("55GB at 150MB/s").expect("GB MB/s");
        assert_eq!(r.title, "6m 07s");
        assert_eq!(r.conversion.as_ref().unwrap().left_badge, "download");
        let r = try_quickwin("55GB at 150MBps").expect("MBps");
        assert_eq!(r.title, "6m 07s");
        // megabit speed: 150 Mbps = 18.75 MB/s → ~48m 53s
        let r = try_quickwin("55GB at 150Mbps").expect("Mbps");
        assert_eq!(r.title, "48m 53s");
        let r = try_quickwin("1.5gb at 100mbps").expect("lowercase mbps");
        assert_eq!(r.title, "2m 00s");
        let r = try_quickwin("150MB/s for 55GB").expect("for order");
        assert_eq!(r.title, "6m 07s");
        let r = try_quickwin("10 GB at 2 Gbps").expect("gbps");
        assert_eq!(r.title, "40s");
        let r = try_quickwin("100MB at 1mbps").expect("slow");
        assert_eq!(r.title, "13m 20s");
    }

    #[test]
    fn speed_unit_case_classifies_bytes_vs_bits() {
        // (unit, table magnitude, is_bytes): lowercase `b` family = bits,
        // capital-`B` forms and explicit byte words = bytes.
        let cases: &[(&str, f64, bool)] = &[
            ("MB", 1e6, true),
            ("MB/s", 1e6, true),
            ("MBPS", 1e6, true),
            ("Mb", 1e6, false),
            ("mb", 1e6, false),
            ("mbps", 1e6, false),
            ("KB", 1e3, true),
            ("Kb", 1e3, false),
            ("kB", 1e3, true),
            ("KiB", 1024.0, true),
            ("MiB", 1_048_576.0, true),
            ("GB", 1e9, true),
            ("Gb", 1e9, false),
            ("TB", 1e12, true),
            ("b", 1.0, false),
            ("B", 1.0, true),
            ("bit", 1.0, false),
            ("kbit", 1e3, false),
            ("mbit", 1e6, false),
            ("gbit", 1e9, false),
            ("Byte", 1.0, true),
            ("Bytes", 1.0, true),
            ("Kilobyte/s", 1e3, true),
            ("kilobytes/s", 1e3, true),
            ("MBytes", 1e6, true),
        ];
        for (u, mag, bytes) in cases {
            let got = parse_speed_bps(&format!("1{u}")).unwrap_or_else(|| panic!("parse 1{u}"));
            let want = if *bytes { *mag } else { *mag * 0.125 };
            assert!(
                (got - want).abs() <= want * 1e-9,
                "1{u} → {got} bps, want {want} ({})",
                if *bytes { "bytes" } else { "bits" }
            );
        }
    }

    #[test]
    fn random_and_coin() {
        let r = try_quickwin("dice").expect("dice");
        let v: i64 = r.title.parse().unwrap();
        assert!((1..=6).contains(&v));
        let r = try_quickwin("roll d20").expect("roll");
        let v: i64 = r.title.parse().unwrap();
        assert!((1..=20).contains(&v));
        let r = try_quickwin("coin").expect("coin");
        assert!(r.title == "heads" || r.title == "tails");
        let r = try_quickwin("random 6").expect("random n");
        let v: i64 = r.title.parse().unwrap();
        assert!((1..=6).contains(&v));
        let r = try_quickwin("random 5 7").expect("random ab");
        let v: i64 = r.title.parse().unwrap();
        assert!((5..=7).contains(&v));
        assert!(try_quickwin("random 0").is_none());
    }

    #[test]
    fn random_full_range_safe() {
        let r = try_quickwin("random -9223372036854775808 9223372036854775807")
            .expect("full i64 range");
        let v: i64 = r.title.parse().unwrap();
        assert!((i64::MIN..=i64::MAX).contains(&v));
        // Subrange draws stay in bounds and eventually vary.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let v = rng_int(-5, 5);
            assert!((-5..=5).contains(&v));
            seen.insert(v);
        }
        assert!(seen.len() > 1, "subrange never varied");
    }

    #[test]
    fn csprng_bytes_differs() {
        let a = csprng_bytes(32).expect("urandom");
        let b = csprng_bytes(32).expect("urandom");
        assert_eq!(a.len(), 32);
        assert_ne!(a, b, "two 32-byte draws identical");
    }

    #[test]
    fn uuid_and_password() {
        let r = try_quickwin("uuid").expect("uuid");
        let s = &r.title;
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
        assert_eq!(&s[14..15], "4", "version nibble");
        let re =
            Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
                .unwrap();
        assert!(re.is_match(s), "bad uuid format: {s}");
        let r = try_quickwin("password 20").expect("password");
        assert_eq!(r.title.len(), 20);
        assert!(r.title.bytes().all(|b| b.is_ascii_alphanumeric()));
        // Length honored (default/clamped) + charset respected.
        for q in ["password", "password 4", "password 128"] {
            let r = try_quickwin(q).expect(q);
            let want = match q.split_once(' ') {
                Some((_, n)) => n.parse::<usize>().unwrap(),
                None => 16,
            };
            assert_eq!(r.title.len(), want, "{q}");
            assert!(
                r.title.chars().all(|c| PASSWORD_CHARS.contains(&(c as u8))),
                "{q}"
            );
        }
    }

    #[test]
    fn text_utils() {
        let r = try_quickwin("wc hello brave world").expect("wc");
        assert_eq!(r.title, "3 words");
        let r = try_quickwin("slug Hello, World!").expect("slug");
        assert_eq!(r.title, "hello-world");
        let r = try_quickwin("case snake Hello World").expect("snake");
        assert_eq!(r.title, "hello_world");
        let r = try_quickwin("case pascal hello world").expect("pascal");
        assert_eq!(r.title, "HelloWorld");
        let r = try_quickwin("case camel hello world").expect("camel");
        assert_eq!(r.title, "helloWorld");
        let r = try_quickwin("case title hello world").expect("title");
        assert_eq!(r.title, "Hello World");
    }

    #[test]
    fn cards_carry_conversion() {
        for q in [
            "255 to hex",
            "ff to dec",
            "roman 1984",
            "bmi 180cm 75kg",
            "10000 steps in km",
            "dice",
            "coin",
            "uuid",
            "password 16",
            "wc hello world",
            "slug Hello World",
            "case snake Hello World",
        ] {
            let r = try_quickwin(q).expect(q);
            assert!(r.conversion.is_some(), "{q}");
        }
    }
}
