//! Small expression evaluator for launcher math.
//!
//! Replaces abandoned `meval` (which pulled ancient `nom` with future-incompat lints).
//! Supports: + - * / % ** ^, unary ±, parentheses, factorial postfix `!`,
//! constants `pi`/`e`, functions `sqrt sin cos tan log ln abs floor ceil round`,
//! and magnitude suffixes (`5k`, `1.5m`, `2 billion`, …).

use std::f64::consts::{E, PI};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char), // + - * / % ^ ! ( ) ,
}

pub(crate) fn eval_str(input: &str) -> Option<f64> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return None;
    }
    let mut i = 0;
    let v = parse_expr(&tokens, &mut i)?;
    if i != tokens.len() {
        return None;
    }
    if !v.is_finite() {
        return None;
    }
    Some(v)
}

fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // multi-char **
        if c == '*' && i + 1 < bytes.len() && bytes[i + 1] as char == '*' {
            out.push(Tok::Op('^'));
            i += 2;
            continue;
        }
        match c {
            '+' | '-' | '*' | '/' | '%' | '^' | '!' | '(' | ')' | ',' => {
                out.push(Tok::Op(c));
                i += 1;
            }
            '0'..='9' | '.' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let d = bytes[i] as char;
                    if d.is_ascii_digit() || d == '.' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                // Scientific notation only when `e`/`E` is followed by digits
                // (optional sign). Avoids treating a bare trailing `e` as part of
                // the number (and leaves room for magnitude suffixes).
                if i < bytes.len() && matches!(bytes[i] as char, 'e' | 'E') {
                    let mut j = i + 1;
                    if j < bytes.len() && matches!(bytes[j] as char, '+' | '-') {
                        j += 1;
                    }
                    let exp_start = j;
                    while j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
                        j += 1;
                    }
                    if j > exp_start {
                        i = j;
                    }
                }
                let mut num: f64 = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
                let (scaled, ni) = apply_magnitude_suffix(bytes, i, num)?;
                num = scaled;
                i = ni;
                out.push(Tok::Num(num));
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let d = bytes[i] as char;
                    if d.is_ascii_alphanumeric() || d == '_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let id = std::str::from_utf8(&bytes[start..i])
                    .ok()?
                    .to_ascii_lowercase();
                out.push(Tok::Ident(id));
            }
            _ => return None,
        }
    }
    Some(out)
}

// Pratt / recursive descent: expr → term → factor → unary → primary
fn parse_expr(tokens: &[Tok], i: &mut usize) -> Option<f64> {
    parse_add(tokens, i)
}

fn parse_add(tokens: &[Tok], i: &mut usize) -> Option<f64> {
    let mut left = parse_mul(tokens, i)?;
    while let Some(Tok::Op(op)) = tokens.get(*i) {
        if *op != '+' && *op != '-' {
            break;
        }
        *i += 1;
        let right = parse_mul(tokens, i)?;
        left = if *op == '+' {
            left + right
        } else {
            left - right
        };
    }
    Some(left)
}

fn parse_mul(tokens: &[Tok], i: &mut usize) -> Option<f64> {
    let mut left = parse_pow(tokens, i)?;
    while let Some(Tok::Op(op)) = tokens.get(*i) {
        if *op != '*' && *op != '/' && *op != '%' {
            break;
        }
        *i += 1;
        let right = parse_pow(tokens, i)?;
        left = match *op {
            '*' => left * right,
            '/' => {
                if right == 0.0 {
                    return None;
                }
                left / right
            }
            '%' => left % right,
            _ => unreachable!(),
        };
    }
    Some(left)
}

/// Right-associative power, then postfix factorial.
fn parse_pow(tokens: &[Tok], i: &mut usize) -> Option<f64> {
    let mut left = parse_unary(tokens, i)?;
    // postfix !
    while matches!(tokens.get(*i), Some(Tok::Op('!'))) {
        *i += 1;
        left = factorial(left)?;
    }
    if matches!(tokens.get(*i), Some(Tok::Op('^'))) {
        *i += 1;
        let right = parse_pow(tokens, i)?; // right-assoc
        left = left.powf(right);
        // more postfix after power result? rare; allow !
        while matches!(tokens.get(*i), Some(Tok::Op('!'))) {
            *i += 1;
            left = factorial(left)?;
        }
    }
    Some(left)
}

fn parse_unary(tokens: &[Tok], i: &mut usize) -> Option<f64> {
    match tokens.get(*i) {
        Some(Tok::Op('+')) => {
            *i += 1;
            parse_unary(tokens, i)
        }
        Some(Tok::Op('-')) => {
            *i += 1;
            Some(-parse_unary(tokens, i)?)
        }
        _ => parse_primary(tokens, i),
    }
}

fn parse_primary(tokens: &[Tok], i: &mut usize) -> Option<f64> {
    match tokens.get(*i).cloned() {
        Some(Tok::Num(n)) => {
            *i += 1;
            Some(n)
        }
        Some(Tok::Ident(name)) => {
            *i += 1;
            // function call
            if matches!(tokens.get(*i), Some(Tok::Op('('))) {
                *i += 1;
                let mut args = Vec::new();
                if !matches!(tokens.get(*i), Some(Tok::Op(')'))) {
                    loop {
                        args.push(parse_expr(tokens, i)?);
                        match tokens.get(*i) {
                            Some(Tok::Op(',')) => {
                                *i += 1;
                                continue;
                            }
                            Some(Tok::Op(')')) => break,
                            _ => return None,
                        }
                    }
                }
                if !matches!(tokens.get(*i), Some(Tok::Op(')'))) {
                    return None;
                }
                *i += 1;
                call_fn(&name, &args)
            } else {
                const_val(&name)
            }
        }
        Some(Tok::Op('(')) => {
            *i += 1;
            let v = parse_expr(tokens, i)?;
            if !matches!(tokens.get(*i), Some(Tok::Op(')'))) {
                return None;
            }
            *i += 1;
            Some(v)
        }
        _ => None,
    }
}

fn const_val(name: &str) -> Option<f64> {
    match name {
        "pi" => Some(PI),
        "e" => Some(E),
        _ => None,
    }
}

fn call_fn(name: &str, args: &[f64]) -> Option<f64> {
    match (name, args) {
        ("sqrt", [x]) => Some(x.sqrt()),
        ("sin", [x]) => Some(x.sin()),
        ("cos", [x]) => Some(x.cos()),
        ("tan", [x]) => Some(x.tan()),
        ("asin", [x]) => Some(x.asin()),
        ("acos", [x]) => Some(x.acos()),
        ("atan", [x]) => Some(x.atan()),
        ("log" | "log10", [x]) => Some(x.log10()),
        ("ln", [x]) => Some(x.ln()),
        ("log2", [x]) => Some(x.log2()),
        ("abs", [x]) => Some(x.abs()),
        ("floor", [x]) => Some(x.floor()),
        ("ceil", [x]) => Some(x.ceil()),
        ("round", [x]) => Some(x.round()),
        ("exp", [x]) => Some(x.exp()),
        ("pow", [a, b]) => Some(a.powf(*b)),
        ("min", [a, b]) => Some(a.min(*b)),
        ("max", [a, b]) => Some(a.max(*b)),
        _ => None,
    }
}

/// Scale factor for finance-style magnitude words/letters after a number.
///
/// Short letters: `k`/`m`/`b`/`t` (and `bn`/`tn`). Full words: thousand…trillion.
/// Unknown words return `None` so unit tokens like `km` are left alone.
fn magnitude_factor(word: &str) -> Option<f64> {
    match word {
        "k" | "thousand" | "thousands" => Some(1_000.0),
        "m" | "mil" | "million" | "millions" => Some(1_000_000.0),
        "b" | "bn" | "billion" | "billions" => Some(1_000_000_000.0),
        "t" | "tn" | "trillion" | "trillions" => Some(1_000_000_000_000.0),
        "hundred" | "hundreds" => Some(100.0),
        // Common South-Asian scales (optional nicety).
        "lakh" | "lac" | "lakhs" | "lacs" => Some(100_000.0),
        "crore" | "crores" => Some(10_000_000.0),
        _ => None,
    }
}

/// After a numeric literal, consume an optional magnitude suffix.
///
/// Accepts glued (`5k`, `1.5m`) and spaced (`2 million`) forms. Does not steal
/// multi-letter unit tokens (`10km` → no suffix match).
fn apply_magnitude_suffix(bytes: &[u8], i: usize, num: f64) -> Option<(f64, usize)> {
    // Allow optional whitespace before word forms (`5 million`).
    let mut j = i;
    while j < bytes.len() && (bytes[j] as char).is_ascii_whitespace() {
        j += 1;
    }
    if j >= bytes.len() {
        return Some((num, i));
    }
    let c = bytes[j] as char;
    if !c.is_ascii_alphabetic() {
        return Some((num, i));
    }

    let start = j;
    j += 1;
    while j < bytes.len() && (bytes[j] as char).is_ascii_alphabetic() {
        j += 1;
    }
    let word = std::str::from_utf8(&bytes[start..j])
        .ok()?
        .to_ascii_lowercase();

    match magnitude_factor(&word) {
        Some(factor) => Some((num * factor, j)),
        // Leave the cursor unmoved so a later Ident token can take over
        // (or the parse fails cleanly on unknown trailing text).
        None => Some((num, i)),
    }
}

fn factorial(x: f64) -> Option<f64> {
    if x < 0.0 || x > 170.0 {
        return None;
    }
    // exact for near-integers
    let n = x.round();
    if (x - n).abs() > 1e-9 {
        return None;
    }
    let n = n as u32;
    let mut acc = 1.0f64;
    for k in 2..=n {
        acc *= k as f64;
    }
    Some(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_ops() {
        assert!((eval_str("2+2").unwrap() - 4.0).abs() < 1e-12);
        assert!((eval_str("10/4").unwrap() - 2.5).abs() < 1e-12);
        assert!((eval_str("2^10").unwrap() - 1024.0).abs() < 1e-9);
        assert!((eval_str("2**10").unwrap() - 1024.0).abs() < 1e-9);
        assert!((eval_str("5!").unwrap() - 120.0).abs() < 1e-9);
        assert!((eval_str("-3+5").unwrap() - 2.0).abs() < 1e-12);
        assert!((eval_str("(1+2)*3").unwrap() - 9.0).abs() < 1e-12);
    }

    #[test]
    fn funcs_and_const() {
        assert!((eval_str("sqrt(144)").unwrap() - 12.0).abs() < 1e-12);
        assert!((eval_str("pi").unwrap() - PI).abs() < 1e-12);
        assert!((eval_str("log(100)").unwrap() - 2.0).abs() < 1e-12);
        assert!((eval_str("abs(-7)").unwrap() - 7.0).abs() < 1e-12);
    }

    #[test]
    fn rejects_bad() {
        assert!(eval_str("").is_none());
        assert!(eval_str("2+").is_none());
        assert!(eval_str("1/0").is_none());
        assert!(eval_str("nope").is_none());
    }

    #[test]
    fn magnitude_suffixes() {
        assert!((eval_str("5k").unwrap() - 5_000.0).abs() < 1e-9);
        assert!((eval_str("1.5k").unwrap() - 1_500.0).abs() < 1e-9);
        assert!((eval_str("2K").unwrap() - 2_000.0).abs() < 1e-9);
        assert!((eval_str("3m").unwrap() - 3_000_000.0).abs() < 1e-6);
        assert!((eval_str("1.5 million").unwrap() - 1_500_000.0).abs() < 1e-6);
        assert!((eval_str("2 billion").unwrap() - 2_000_000_000.0).abs() < 1e-3);
        assert!((eval_str("1t").unwrap() - 1_000_000_000_000.0).abs() < 1.0);
        assert!((eval_str("1 trillion").unwrap() - 1e12).abs() < 1.0);
        assert!((eval_str("2bn").unwrap() - 2e9).abs() < 1e-3);
        // Expressions
        assert!((eval_str("5k + 2k").unwrap() - 7_000.0).abs() < 1e-9);
        assert!((eval_str("1m / 4").unwrap() - 250_000.0).abs() < 1e-6);
        assert!((eval_str("10k * 3").unwrap() - 30_000.0).abs() < 1e-9);
        assert!((eval_str("(2.5m + 500k) / 1k").unwrap() - 3_000.0).abs() < 1e-6);
        // Scientific still works; not confused with magnitude
        assert!((eval_str("1e3").unwrap() - 1_000.0).abs() < 1e-12);
        assert!((eval_str("1e3 + 1k").unwrap() - 2_000.0).abs() < 1e-9);
        // Unit-like tokens must not be partially eaten as magnitude
        assert!(eval_str("10km").is_none());
    }
}
