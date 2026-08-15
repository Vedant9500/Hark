//! Unit arithmetic engine (T1): same-dimension add/sub, ×/÷ by a bare number,
//! `% of` / `tip % on` quantities, fractions (`1/2 cup`) and compound speed
//! (`5km / 2h`, `60km/h * 2`). Everything renders on the Raycast-style card.
//!
//! Routing rule: the duration provider still owns time arithmetic (`2h + 30m`,
//! `1h 30min * 2`); this engine covers every other dimension (length, mass,
//! data, volume, area, speed). Single-letter `m`/`b`/`t` are units here.

use super::units::{normalize_unit, to_base};
use super::util::{card_result, format_number, parse_qty_number};
use crate::providers::SearchResult;
use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Clone)]
struct Qty {
    /// Value in base unit (m, g, b, l, m², m/s, s…).
    base: f64,
    /// Category from `units::to_base`; `None` = dimensionless.
    cat: Option<&'static str>,
    /// Original canonical unit for display (e.g. "cup", "km/h").
    unit: Option<String>,
    /// Precomputed display (speed from unit division).
    display: Option<String>,
}

pub(crate) fn try_unit_math(q: &str) -> Option<SearchResult> {
    if let Some(r) = try_pct_units(q) {
        return Some(r);
    }
    if let Some(r) = try_tip_units(q) {
        return Some(r);
    }

    let norm = q.replace('×', "*").replace('÷', "/");
    let (qty, had_op, unitful, had_fraction) = parse_expr(&norm)?;
    if !unitful {
        // Pure-number expression → plain math owns it.
        return None;
    }
    // Bare unit values without arithmetic ("5km", "100m") are P2; only a bare
    // fraction quantity ("1/2 cup") is shown here.
    if !had_op && !had_fraction {
        return None;
    }

    let formatted = if !had_op && had_fraction {
        // Show a bare fraction in its original unit ("0.5 cup").
        let v = qty.base / unit_factor(qty.unit.as_deref()?)?;
        format!("{} {}", format_number(v), qty.unit.as_deref()?)
    } else {
        display_qty(&qty)
    };
    let shown = q.trim().to_string();
    Some(card_result(
        formatted.clone(),
        format!("= {shown}"),
        formatted.clone(),
        shown,
        "expression",
        formatted,
        "result",
    ))
}

/// `N% of QTY` → QTY × N/100 (unit-aware; pure-number targets stay in math).
fn try_pct_units(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*([+-]?\d+(?:\.\d+)?)\s*%\s*of\s*(.+?)\s*$").unwrap()
    });
    let c = RE.captures(q)?;
    let pct: f64 = c.get(1)?.as_str().parse().ok()?;
    let (qty, _, unitful, _) = parse_expr(c.get(2)?.as_str())?;
    if !unitful {
        return None;
    }
    let mut out = qty.clone();
    out.base = qty.base * pct / 100.0;
    out.display = None;
    let formatted = display_qty(&out);
    let shown = c.get(2)?.as_str().trim();
    Some(card_result(
        formatted.clone(),
        format!("{pct}% of {shown}"),
        formatted.clone(),
        format!("{pct}% of {shown}"),
        "percentage",
        formatted,
        "result",
    ))
}

/// `tip N% on QTY` → total including tip (unit-aware).
fn try_tip_units(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*tip\s+([+-]?\d+(?:\.\d+)?)\s*%\s*(?:on|for)\s*(.+?)\s*$").unwrap()
    });
    let c = RE.captures(q)?;
    let pct: f64 = c.get(1)?.as_str().parse().ok()?;
    let (qty, _, unitful, _) = parse_expr(c.get(2)?.as_str())?;
    if !unitful {
        return None;
    }
    let shown = c.get(2)?.as_str().trim();
    let mut tip = qty.clone();
    tip.base = qty.base * pct / 100.0;
    tip.display = None;
    let tip_disp = display_qty(&tip);
    let mut total = qty.clone();
    total.base = qty.base * (1.0 + pct / 100.0);
    total.display = None;
    let total_disp = display_qty(&total);
    let title = format!("Total {total_disp}");
    Some(card_result(
        title.clone(),
        format!("Tip {tip_disp} on {shown}"),
        total_disp,
        q.to_string(),
        "tip",
        title,
        "result",
    ))
}

fn unit_factor(u: &str) -> Option<f64> {
    to_base(&normalize_unit(u)).map(|(f, _)| f)
}

fn is_unit(u: &str) -> bool {
    to_base(&normalize_unit(u)).is_some()
}

/// Parse a full expression (terms with + - * /), left-to-right with */ tighter.
fn parse_expr(s: &str) -> Option<(Qty, bool, bool, bool)> {
    let mut p = P::new(s);
    let (qty, op, unitful, frac) = parse_add(&mut p)?;
    p.skip_ws();
    if p.i != p.chars.len() {
        return None;
    }
    Some((qty, op, unitful, frac))
}

struct P<'a> {
    _b: &'a [u8],
    chars: Vec<char>,
    i: usize,
}

impl<'a> P<'a> {
    fn new(s: &'a str) -> Self {
        P { _b: s.as_bytes(), chars: s.chars().collect(), i: 0 }
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }
    fn peek2(&self) -> Option<char> {
        self.chars.get(self.i + 1).copied()
    }
}

fn parse_add(p: &mut P) -> Option<(Qty, bool, bool, bool)> {
    let (mut acc, mut op, mut unitful, mut frac) = parse_mul(p)?;
    loop {
        p.skip_ws();
        match p.peek() {
            Some('+') | Some('-') => {}
            _ => break,
        }
        let c = p.peek()?;
        p.i += 1;
        let (rhs, _, u2, f2) = parse_mul(p)?;
        op = true;
        unitful |= u2;
        frac |= f2;
        acc = add_sub(acc, rhs, c)?;
    }
    Some((acc, op, unitful, frac))
}

fn parse_mul(p: &mut P) -> Option<(Qty, bool, bool, bool)> {
    let (mut acc, mut unitful, mut frac) = parse_term(p)?;
    let mut op = false;
    loop {
        p.skip_ws();
        match p.peek() {
            Some('*') | Some('/') => {}
            _ => break,
        }
        let c = p.peek()?;
        p.i += 1;
        let (rhs, u2, f2) = parse_term(p)?;
        op = true;
        unitful |= u2;
        frac |= f2;
        acc = mul_div(acc, rhs, c)?;
    }
    Some((acc, op, unitful, frac))
}

/// Parse `[+-] number[/number] [unit][/unit]`.
/// Returns `(Qty, had_op=false, unitful, had_fraction)`.
fn parse_term(p: &mut P) -> Option<(Qty, bool, bool)> {
    p.skip_ws();
    let mut neg = false;
    match p.peek()? {
        '+' => p.i += 1,
        '-' => {
            p.i += 1;
            neg = true;
        }
        _ => {}
    }

    // Number: fraction (`1/2`) or decimal. A `/` after digits only counts as a
    // fraction when it's followed by another digit (else it's division).
    let mut num_s = String::new();
    while let Some(c) = p.peek() {
        if c.is_ascii_digit() || c == '.' {
            num_s.push(c);
            p.i += 1;
        } else {
            break;
        }
    }
    let mut had_fraction = false;
    if p.peek() == Some('/') && matches!(p.peek2(), Some(c) if c.is_ascii_digit()) {
        p.i += 1;
        num_s.push('/');
        while let Some(c) = p.peek() {
            if c.is_ascii_digit() || c == '.' {
                num_s.push(c);
                p.i += 1;
            } else {
                break;
            }
        }
        had_fraction = true;
    }
    if num_s.is_empty() {
        return None;
    }
    let num = parse_qty_number(&num_s)?;
    p.skip_ws();

    // Unit: letters (with optional ²-style trailing digit), then optional /unit.
    let mut u = String::new();
    loop {
        match p.peek() {
            Some(c) if c.is_ascii_alphabetic() || matches!(c, 'µ' | 'μ' | '°' | '²' | '³') => {
                u.push(c);
                p.i += 1;
            }
            Some(c) if c.is_ascii_digit() => {
                let test = format!("{u}{c}");
                if is_unit(&test) {
                    u.push(c);
                    p.i += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    let mut den = String::new();
    if !u.is_empty() && p.peek() == Some('/') && matches!(p.peek2(), Some(c) if c.is_ascii_alphabetic()) {
        p.i += 1;
        loop {
            match p.peek() {
                Some(c) if c.is_ascii_alphabetic() || matches!(c, 'µ' | 'μ' | '°' | '²' | '³') => {
                    den.push(c);
                    p.i += 1;
                }
                Some(c) if c.is_ascii_digit() => {
                    let test = format!("{den}{c}");
                    if is_unit(&test) {
                        den.push(c);
                        p.i += 1;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    let mut value = if neg { -num } else { num };

    if u.is_empty() {
        return Some((
            Qty { base: value, cat: None, unit: None, display: None },
            false,
            had_fraction,
        ));
    }
    if !den.is_empty() {
        // Compound unit: only length/time → speed (km/h, m/s…).
        let (fn_, cnum) = to_base(&normalize_unit(&u))?;
        let (fd, cden) = to_base(&normalize_unit(&den))?;
        if cnum != "length" || cden != "time" {
            return None;
        }
        value *= fn_ / fd;
        return Some((
            Qty {
                base: value,
                cat: Some("speed"),
                unit: Some(format!("{u}/{den}")),
                display: None,
            },
            true,
            had_fraction,
        ));
    }

    let (factor, cat) = to_base(&normalize_unit(&u))?;
    Some((
        Qty { base: value * factor, cat: Some(cat), unit: Some(u), display: None },
        true,
        had_fraction,
    ))
}

fn add_sub(a: Qty, b: Qty, op: char) -> Option<Qty> {
    if a.cat.is_none() || b.cat.is_none() || a.cat != b.cat {
        return None;
    }
    let base = if op == '+' { a.base + b.base } else { a.base - b.base };
    Some(Qty { base, cat: a.cat, unit: None, display: None })
}

fn mul_div(a: Qty, b: Qty, op: char) -> Option<Qty> {
    match op {
        '*' => {
            if a.cat.is_none() {
                Some(Qty { base: a.base * b.base, cat: b.cat, unit: b.unit, display: None })
            } else if b.cat.is_none() {
                Some(Qty { base: a.base * b.base, cat: a.cat, unit: a.unit, display: None })
            } else {
                None
            }
        }
        '/' => {
            if b.cat.is_none() {
                if b.base == 0.0 {
                    return None;
                }
                Some(Qty { base: a.base / b.base, cat: a.cat, unit: a.unit, display: None })
            } else if a.cat.is_none() {
                None
            } else if a.cat == Some("length") && b.cat == Some("time") {
                let unit_a = a.unit.clone().unwrap_or_else(|| "m".into());
                let b_secs = b.base;
                let af = unit_factor(&unit_a)?;
                let display = if matches!(b.unit.as_deref(), Some("s") | Some("ms")) {
                    let v = (a.base / af) / b_secs;
                    format!("{} {}/s", format_number(v), unit_a)
                } else {
                    let v = (a.base / af) / (b_secs / 3600.0);
                    format!("{} {}/h", format_number(v), unit_a)
                };
                Some(Qty {
                    base: a.base / b.base,
                    cat: Some("speed"),
                    unit: None,
                    display: Some(display),
                })
            } else if a.cat == b.cat {
                if b.base == 0.0 {
                    return None;
                }
                Some(Qty { base: a.base / b.base, cat: None, unit: None, display: None })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn display_qty(q: &Qty) -> String {
    if let Some(d) = &q.display {
        return d.clone();
    }
    match q.cat {
        None => format_number(q.base),
        Some("time") => super::duration::format_duration(q.base),
        Some(cat) => {
            if let Some(u) = &q.unit {
                if let Some(slash) = u.find('/') {
                    if let (Some(fn_), Some(fd)) = (
                        unit_factor(&u[..slash]),
                        unit_factor(&u[slash + 1..]),
                    ) {
                        return format!("{} {}", format_number(q.base / (fn_ / fd)), u);
                    }
                }
            }
            smart_prefix(q.base, cat)
        }
    }
}

/// Pick the prefix that lands the value ≥ 1 (largest first); falls back to the
/// smallest prefix. `2km/5` → "400 m", `200mb * 10` → "2 gb".
fn smart_prefix(base: f64, cat: &str) -> String {
    let list: &[&str] = match cat {
        "length" => &["km", "m", "cm", "mm"],
        "mass" => &["t", "kg", "g", "mg"],
        "data" => &["tb", "gb", "mb", "kb", "b"],
        "volume" => &["gal", "l", "pt", "cup", "ml", "tbsp", "tsp"],
        "area" => &["km2", "ha", "acre", "m2", "ft2", "cm2", "mm2"],
        "speed" => &["km/h", "m/s"],
        _ => return format!("{} {cat}", format_number(base)),
    };
    let abs = base.abs();
    let mut chosen = *list.last().expect("non-empty");
    for u in list {
        if let Some((f, _)) = to_base(u) {
            if abs / f >= 1.0 {
                chosen = u;
                break;
            }
        }
    }
    let f = to_base(chosen).expect("known unit").0;
    format!("{} {}", format_number(base / f), chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(q: &str) -> String {
        try_unit_math(q).expect(q).title
    }

    #[test]
    fn mul_div_by_number() {
        assert_eq!(t("200mb * 10"), "2 gb");
        assert_eq!(t("2km / 5"), "400 m");
        assert_eq!(t("2km/5"), "400 m");
        assert_eq!(t("2km ÷ 5"), "400 m");
        assert_eq!(t("1kg * 4"), "4 kg");
        assert_eq!(t("500g / 2"), "250 g");
        assert_eq!(t("2km×3"), "6 km");
        assert_eq!(t("5 * 2km"), "10 km");
        assert_eq!(t("100m / 2"), "50 m");
        assert_eq!(t("1m * 3"), "3 m");
    }

    #[test]
    fn add_sub_same_dimension() {
        assert_eq!(t("2m + 30cm"), "2.3 m");
        assert_eq!(t("1km + 500m"), "1.5 km");
        assert_eq!(t("5km + 2km"), "7 km");
        assert_eq!(t("2km - 500m"), "1.5 km");
        assert_eq!(t("200mb + 100mb"), "300 mb");
        assert_eq!(t("1gb - 512mb"), "488 mb");
        assert_eq!(t("100m + 5m"), "105 m");
    }

    #[test]
    fn pct_and_tip_of_units() {
        assert_eq!(t("15% of 2km"), "300 m");
        assert_eq!(t("10% of 200mb"), "20 mb");
        assert_eq!(t("50% of 2h"), "1h");
        assert_eq!(t("tip 10% on 500g"), "Total 550 g");
    }

    #[test]
    fn fractions() {
        assert_eq!(t("1/2 cup"), "0.5 cup");
    }

    #[test]
    fn compound_units() {
        assert_eq!(t("5km / 2h"), "2.5 km/h");
        assert_eq!(t("60km/h * 2"), "120 km/h");
        assert_eq!(t("2km² / 2"), "1 km2");
        assert_eq!(t("4m2 * 3"), "12 m2");
    }

    #[test]
    fn does_not_steal() {
        // Pure math stays in math provider.
        assert!(try_unit_math("2+2").is_none());
        assert!(try_unit_math("5k + 2k").is_none());
        assert!(try_unit_math("1/2").is_none());
        // Bare unit values (P2) stay unhandled here.
        assert!(try_unit_math("5km").is_none());
        assert!(try_unit_math("100m").is_none());
        // Multi-token time → duration provider.
        assert!(try_unit_math("2h + 30m").is_none());
        assert!(try_unit_math("10h 30min").is_none());
        // Pure-number %/tip → math provider.
        assert!(try_unit_math("10% of 2k").is_none());
        assert!(try_unit_math("tip 15% on 2k").is_none());
        // Dates → datetime provider.
        assert!(try_unit_math("2026-08-15").is_none());
        // Temperature → conversion provider (no arithmetic).
        assert!(try_unit_math("20 c + 3 c").is_none());
    }
}
