use super::util::format_number;
use crate::providers::fx::is_currency;
use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) static RE_CONVERT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)^\s*([+-]?\d+(?:\.\d+)?)\s*",
        r"([a-zA-Z°²³/µμ]+(?:\^[23])?)\s+",
        r"(?:to|in|as|->|→)\s+",
        r"([a-zA-Z°²³/µμ]+(?:\^[23])?)?\s*$",
    ))
    .unwrap()
});

// Incomplete: "10kg to pou" / "10 kg to" (target optional/partial)
pub(crate) static RE_CONVERT_PARTIAL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)^\s*([+-]?\d+(?:\.\d+)?)\s*",
        r"([a-zA-Z°²³/µμ]+(?:\^[23])?)\s+",
        r"(to|in|as|->|→)\s*",
        r"([a-zA-Z°²³/µμ]*)\s*$",
    ))
    .unwrap()
});

pub(crate) fn try_conversion(q: &str) -> Option<SearchResult> {
    let caps = RE_CONVERT.captures(q)?;
    let value: f64 = caps.get(1)?.as_str().parse().ok()?;
    let from_raw = caps.get(2)?.as_str();
    let to_raw = caps.get(3)?.as_str();
    if to_raw.is_empty() {
        return None;
    }
    // Skip pure currency pairs (handled by FX)
    if is_currency(from_raw) && is_currency(to_raw) {
        return None;
    }
    let from = resolve_unit(from_raw)?;
    let to = resolve_unit(to_raw)?;
    unit_result(value, &from, &to)
}

/// Predict incomplete targets: `10kg to pou` → pounds, `100m to ki` → km
pub(crate) fn try_conversion_predict(q: &str) -> Option<Vec<SearchResult>> {
    let caps = RE_CONVERT_PARTIAL.captures(q)?;
    let value: f64 = caps.get(1)?.as_str().parse().ok()?;
    let from_raw = caps.get(2)?.as_str();
    let to_prefix = caps.get(4).map(|m| m.as_str()).unwrap_or("").trim();

    // Don't steal currency queries
    if is_currency(from_raw) {
        return None;
    }
    // Exact unit already handled
    if !to_prefix.is_empty() && resolve_unit(to_prefix).is_some() {
        return None;
    }

    let from = resolve_unit(from_raw)?;
    let from_cat = to_base(&from)?.1;
    let mut targets = predict_units(to_prefix, from_cat, &from);
    if targets.is_empty() {
        return None;
    }
    targets.truncate(4);

    let mut out = Vec::new();
    for (i, to) in targets.into_iter().enumerate() {
        if let Some(mut r) = unit_result(value, &from, &to) {
            // Rank best prediction first
            r.score = 10_000 - (i as i64 * 10);
            // Show predicted unit name in subtitle when partial
            if !to_prefix.is_empty() && !to_prefix.eq_ignore_ascii_case(&to) {
                r.subtitle = format!("{} → predicted “{}”", r.subtitle, unit_display_name(&to));
            }
            out.push(r);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub(crate) fn unit_result(value: f64, from: &str, to: &str) -> Option<SearchResult> {
    let (result, label) = convert(value, from, to)?;
    let formatted = format_number(result);
    let title = format!("{formatted} {to}");
    Some(SearchResult {
        id: format!("conv:{value}:{from}:{to}"),
        title: title.clone(),
        subtitle: format!("{value} {from} → {to} · {label}"),
        kind: ResultKind::Conversion,
        score: 10_000,
        icon: Some("accessories-calculator".into()),
        action: Action::Copy(title.clone()),
        conversion: Some(ConversionView {
            left_title: format!("{value} {from}"),
            left_badge: label.to_string(),
            right_title: title,
            right_badge: to.to_string(),
        }),
    })
}

/// Exact unit resolve (aliases → canonical).
pub(crate) fn resolve_unit(raw: &str) -> Option<String> {
    let n = normalize_unit(raw);
    // normalize_unit returns the input lowercased if unknown — verify via to_base / temp
    if matches!(n.as_str(), "c" | "f" | "k") {
        return Some(n);
    }
    if to_base(&n).is_some() {
        return Some(n);
    }
    None
}

/// Prefix / fuzzy unit prediction within a category.
pub(crate) fn predict_units(prefix: &str, category: &str, from: &str) -> Vec<String> {
    let p = prefix.to_lowercase();
    let mut hits: Vec<(i32, String)> = Vec::new();

    for (alias, canon) in UNIT_ALIASES {
        let cat = match to_base(canon) {
            Some((_, c)) => c,
            None if matches!(*canon, "c" | "f" | "k") => "temperature",
            None => continue,
        };
        if cat != category {
            continue;
        }
        if *canon == from {
            continue;
        }
        let score = if p.is_empty() {
            // Empty target: suggest common defaults
            50
        } else if *alias == p || *canon == p {
            1000
        } else if alias.starts_with(&p) {
            500 - alias.len() as i32
        } else if canon.starts_with(&p) {
            400 - canon.len() as i32
        } else if p.len() >= 2 && alias.contains(&p) {
            200 - alias.len() as i32
        } else {
            continue;
        };
        hits.push((score, (*canon).to_string()));
    }

    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (_, u) in hits {
        if seen.insert(u.clone()) {
            out.push(u);
        }
    }

    // Empty prefix: prefer common targets per category
    if p.is_empty() {
        let preferred: &[&str] = match category {
            "mass" => &["lb", "g", "oz", "t"],
            "length" => &["mi", "ft", "cm", "in"],
            "volume" => &["gal", "ml", "cup"],
            "temperature" => &["f", "c", "k"],
            "speed" => &["mph", "km/h", "kn"],
            "data" => &["mb", "gb", "kib"],
            "time" => &["min", "h", "d"],
            "area" => &["ft2", "acre", "ha"],
            _ => &[],
        };
        let mut ranked = Vec::new();
        for pref in preferred {
            if *pref != from && out.iter().any(|x| x == pref) {
                ranked.push((*pref).to_string());
            }
        }
        for u in out {
            if !ranked.contains(&u) {
                ranked.push(u);
            }
        }
        return ranked;
    }
    out
}

pub(crate) fn unit_display_name(canon: &str) -> String {
    match canon {
        "lb" => "pounds".into(),
        "kg" => "kilograms".into(),
        "g" => "grams".into(),
        "oz" => "ounces".into(),
        "mi" => "miles".into(),
        "km" => "kilometers".into(),
        "ft" => "feet".into(),
        "in" => "inches".into(),
        "m" => "meters".into(),
        "cm" => "centimeters".into(),
        "gal" => "gallons".into(),
        "l" => "liters".into(),
        "ml" => "milliliters".into(),
        "c" => "celsius".into(),
        "f" => "fahrenheit".into(),
        "k" => "kelvin".into(),
        "mph" => "mph".into(),
        "km/h" => "km/h".into(),
        other => other.to_string(),
    }
}

pub(crate) static UNIT_ALIASES: &[(&str, &str)] = &[
    // mass
    ("kg", "kg"),
    ("kilogram", "kg"),
    ("kilograms", "kg"),
    ("kgs", "kg"),
    ("g", "g"),
    ("gram", "g"),
    ("grams", "g"),
    ("mg", "mg"),
    ("milligram", "mg"),
    ("milligrams", "mg"),
    ("lb", "lb"),
    ("lbs", "lb"),
    ("pound", "lb"),
    ("pounds", "lb"),
    ("oz", "oz"),
    ("ounce", "oz"),
    ("ounces", "oz"),
    ("t", "t"),
    ("tonne", "t"),
    ("tonnes", "t"),
    ("st", "st"),
    ("stone", "st"),
    ("stones", "st"),
    // length
    ("m", "m"),
    ("meter", "m"),
    ("meters", "m"),
    ("metre", "m"),
    ("metres", "m"),
    ("km", "km"),
    ("kilometer", "km"),
    ("kilometers", "km"),
    ("kilometre", "km"),
    ("kilometres", "km"),
    ("kms", "km"),
    ("cm", "cm"),
    ("centimeter", "cm"),
    ("centimeters", "cm"),
    ("mm", "mm"),
    ("millimeter", "mm"),
    ("millimeters", "mm"),
    ("mi", "mi"),
    ("mile", "mi"),
    ("miles", "mi"),
    ("ft", "ft"),
    ("foot", "ft"),
    ("feet", "ft"),
    ("in", "in"),
    ("inch", "in"),
    ("inches", "in"),
    ("yd", "yd"),
    ("yard", "yd"),
    ("yards", "yd"),
    ("nmi", "nmi"),
    // volume
    ("l", "l"),
    ("liter", "l"),
    ("liters", "l"),
    ("litre", "l"),
    ("litres", "l"),
    ("ml", "ml"),
    ("milliliter", "ml"),
    ("milliliters", "ml"),
    ("gal", "gal"),
    ("gallon", "gal"),
    ("gallons", "gal"),
    ("cup", "cup"),
    ("cups", "cup"),
    ("pt", "pt"),
    ("pint", "pt"),
    ("pints", "pt"),
    ("qt", "qt"),
    ("quart", "qt"),
    // temp
    ("c", "c"),
    ("celsius", "c"),
    ("centigrade", "c"),
    ("f", "f"),
    ("fahrenheit", "f"),
    ("k", "k"),
    ("kelvin", "k"),
    // speed
    ("mph", "mph"),
    ("km/h", "km/h"),
    ("kph", "km/h"),
    ("kmh", "km/h"),
    ("m/s", "m/s"),
    ("kn", "kn"),
    ("knot", "kn"),
    ("knots", "kn"),
    // time
    ("s", "s"),
    ("sec", "s"),
    ("second", "s"),
    ("seconds", "s"),
    ("min", "min"),
    ("minute", "min"),
    ("minutes", "min"),
    ("h", "h"),
    ("hr", "h"),
    ("hour", "h"),
    ("hours", "h"),
    ("d", "d"),
    ("day", "d"),
    ("days", "d"),
    // data
    ("b", "b"),
    ("byte", "b"),
    ("bytes", "b"),
    ("kb", "kb"),
    ("mb", "mb"),
    ("gb", "gb"),
    ("tb", "tb"),
    ("kib", "kib"),
    ("mib", "mib"),
    ("gib", "gib"),
    // area
    ("m2", "m2"),
    ("km2", "km2"),
    ("ft2", "ft2"),
    ("sqft", "ft2"),
    ("acre", "acre"),
    ("acres", "acre"),
    ("ha", "ha"),
    ("hectare", "ha"),
];

pub(crate) fn normalize_unit(u: &str) -> String {
    let u = u
        .to_lowercase()
        .replace('°', "")
        .replace('µ', "u")
        .replace('μ', "u")
        .replace('²', "2")
        .replace('³', "3")
        .replace('^', "");

    // Prefer alias table for consistency with prediction
    for (alias, canon) in UNIT_ALIASES {
        if *alias == u {
            return (*canon).to_string();
        }
    }

    match u.as_str() {
        "micrometre" | "micrometers" | "micrometres" | "micron" | "microns" => "um".into(),
        "nanometre" | "nanometers" | "nanometres" => "nm".into(),
        "nauticalmile" | "nauticalmiles" => "nmi".into(),
        "micrograms" | "microgram" => "ug".into(),
        "metric ton" | "metric tons" => "t".into(),
        "cubicmeters" | "cubicmetre" | "cubicmeter" => "m3".into(),
        "cubiccentimeters" | "cc" => "cm3".into(),
        "usgal" => "gal".into(),
        "ukgallons" | "ukgallon" | "impgal" => "ukgal".into(),
        "tablespoons" | "tablespoon" | "tbsp" => "tbsp".into(),
        "teaspoons" | "teaspoon" | "tsp" => "tsp".into(),
        "fluidounces" | "fluidounce" | "floz" => "floz".into(),
        "milliseconds" | "millisecond" | "millis" | "msec" => "ms".into(),
        "microseconds" | "microsecond" | "usecs" => "us".into(),
        "weeks" | "week" => "wk".into(),
        "months" | "month" => "mo".into(),
        "years" | "year" | "yrs" | "yr" => "yr".into(),
        "petabytes" | "petabyte" => "pb".into(),
        "tebibytes" | "tebibyte" => "tib".into(),
        "mps" | "meterspersecond" | "metrespersecond" => "m/s".into(),
        "kilometersperhour" | "kilometresperhour" => "km/h".into(),
        "mi/h" | "milesperhour" => "mph".into(),
        "fps" | "ft/s" | "feetpersecond" => "ft/s".into(),
        "kt" | "kts" => "kn".into(),
        "sqm" | "squaremeters" | "squaremetres" => "m2".into(),
        "sqkm" | "squarekilometers" => "km2".into(),
        "sqcm" => "cm2".into(),
        "squarefeet" | "squarefoot" => "ft2".into(),
        "sqin" | "squareinches" => "in2".into(),
        "sqmi" | "squaremiles" => "mi2".into(),
        "pascals" | "pascal" => "pa".into(),
        "kilopascals" | "kilopascal" => "kpa".into(),
        "bars" => "bar".into(),
        "atmospheres" | "atmosphere" | "ats" => "atm".into(),
        "poundspersquareinch" => "psi".into(),
        "torr" => "mmhg".into(),
        "joules" | "joule" => "j".into(),
        "kilojoules" | "kilojoule" => "kj".into(),
        "calories" | "calorie" => "cal".into(),
        "kilocalories" | "kilocalorie" | "caloriesfood" => "kcal".into(),
        "watthours" | "watthour" => "wh".into(),
        "kilowatthours" | "kilowatthour" => "kwh".into(),
        "electronvolts" | "electronvolt" => "ev".into(),
        "btus" => "btu".into(),
        "watts" | "watt" => "w".into(),
        "kilowatts" | "kilowatt" => "kw".into(),
        "megawatts" | "megawatt" => "mw".into(),
        "horsepower" => "hp".into(),
        "degrees" | "degree" => "deg".into(),
        "radians" | "radian" => "rad".into(),
        "hertz" => "hz".into(),
        "kilohertz" => "khz".into(),
        "megahertz" => "mhz".into(),
        "gigahertz" => "ghz".into(),
        "\"" => "in".into(),
        other => other.to_string(),
    }
}

pub(crate) fn convert(value: f64, from: &str, to: &str) -> Option<(f64, &'static str)> {
    if matches!(from, "c" | "f" | "k") && matches!(to, "c" | "f" | "k") {
        let c = match from {
            "c" => value,
            "f" => (value - 32.0) * 5.0 / 9.0,
            "k" => value - 273.15,
            _ => return None,
        };
        let out = match to {
            "c" => c,
            "f" => c * 9.0 / 5.0 + 32.0,
            "k" => c + 273.15,
            _ => return None,
        };
        return Some((out, "temperature"));
    }

    let (from_base, cat) = to_base(from)?;
    let (to_base, cat2) = to_base(to)?;
    if cat != cat2 {
        return None;
    }
    Some((value * from_base / to_base, cat))
}

pub(crate) fn to_base(unit: &str) -> Option<(f64, &'static str)> {
    match unit {
        // length → m
        "m" => Some((1.0, "length")),
        "km" => Some((1000.0, "length")),
        "cm" => Some((0.01, "length")),
        "mm" => Some((0.001, "length")),
        "um" => Some((1e-6, "length")),
        "nm" => Some((1e-9, "length")),
        "mi" => Some((1609.344, "length")),
        "yd" => Some((0.9144, "length")),
        "ft" => Some((0.3048, "length")),
        "in" => Some((0.0254, "length")),
        "nmi" => Some((1852.0, "length")),
        // mass → g
        "g" => Some((1.0, "mass")),
        "kg" => Some((1000.0, "mass")),
        "mg" => Some((0.001, "mass")),
        "ug" => Some((1e-6, "mass")),
        "t" => Some((1_000_000.0, "mass")),
        "lb" => Some((453.59237, "mass")),
        "oz" => Some((28.349523125, "mass")),
        "st" => Some((6350.29318, "mass")),
        // volume → L
        "l" => Some((1.0, "volume")),
        "ml" => Some((0.001, "volume")),
        "m3" => Some((1000.0, "volume")),
        "cm3" => Some((0.001, "volume")),
        "gal" => Some((3.785411784, "volume")),
        "ukgal" => Some((4.54609, "volume")),
        "qt" => Some((0.946352946, "volume")),
        "pt" => Some((0.473176473, "volume")),
        "cup" => Some((0.2365882365, "volume")),
        "tbsp" => Some((0.0147867648, "volume")),
        "tsp" => Some((0.00492892159, "volume")),
        "floz" => Some((0.0295735296, "volume")),
        // time → s
        "s" => Some((1.0, "time")),
        "ms" => Some((0.001, "time")),
        "us" => Some((1e-6, "time")),
        "min" => Some((60.0, "time")),
        "h" => Some((3600.0, "time")),
        "d" => Some((86400.0, "time")),
        "wk" => Some((604800.0, "time")),
        "mo" => Some((2_629_746.0, "time")),
        "yr" => Some((31_556_952.0, "time")),
        // data → bytes (binary for kib etc, decimal for kb)
        "b" => Some((1.0, "data")),
        "kb" => Some((1000.0, "data")),
        "mb" => Some((1_000_000.0, "data")),
        "gb" => Some((1_000_000_000.0, "data")),
        "tb" => Some((1e12, "data")),
        "pb" => Some((1e15, "data")),
        "kib" => Some((1024.0, "data")),
        "mib" => Some((1024.0_f64.powi(2), "data")),
        "gib" => Some((1024.0_f64.powi(3), "data")),
        "tib" => Some((1024.0_f64.powi(4), "data")),
        // speed → m/s
        "m/s" => Some((1.0, "speed")),
        "km/h" => Some((1000.0 / 3600.0, "speed")),
        "mph" => Some((1609.344 / 3600.0, "speed")),
        "ft/s" => Some((0.3048, "speed")),
        "kn" => Some((1852.0 / 3600.0, "speed")),
        // area → m²
        "m2" => Some((1.0, "area")),
        "km2" => Some((1_000_000.0, "area")),
        "cm2" => Some((0.0001, "area")),
        "mm2" => Some((1e-6, "area")),
        "ft2" => Some((0.09290304, "area")),
        "in2" => Some((0.00064516, "area")),
        "acre" => Some((4046.8564224, "area")),
        "ha" => Some((10000.0, "area")),
        "mi2" => Some((2_589_988.110336, "area")),
        // pressure → Pa
        "pa" => Some((1.0, "pressure")),
        "kpa" => Some((1000.0, "pressure")),
        "bar" => Some((100_000.0, "pressure")),
        "atm" => Some((101_325.0, "pressure")),
        "psi" => Some((6894.757293168, "pressure")),
        "mmhg" => Some((133.322387415, "pressure")),
        // energy → J
        "j" => Some((1.0, "energy")),
        "kj" => Some((1000.0, "energy")),
        "cal" => Some((4.184, "energy")),
        "kcal" => Some((4184.0, "energy")),
        "wh" => Some((3600.0, "energy")),
        "kwh" => Some((3_600_000.0, "energy")),
        "ev" => Some((1.602176634e-19, "energy")),
        "btu" => Some((1055.05585262, "energy")),
        // power → W
        "w" => Some((1.0, "power")),
        "kw" => Some((1000.0, "power")),
        "mw" => Some((1_000_000.0, "power")),
        "hp" => Some((745.699871582, "power")),
        // angle → rad
        "rad" => Some((1.0, "angle")),
        "deg" => Some((std::f64::consts::PI / 180.0, "angle")),
        // frequency → Hz
        "hz" => Some((1.0, "frequency")),
        "khz" => Some((1000.0, "frequency")),
        "mhz" => Some((1_000_000.0, "frequency")),
        "ghz" => Some((1_000_000_000.0, "frequency")),
        _ => None,
    }
}
