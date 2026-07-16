//! Small expression evaluator for launcher math.
//!
//! Replaces abandoned `meval` (which pulled ancient `nom` with future-incompat lints).
//! Supports: + - * / % ** ^, unary ±, parentheses, factorial postfix `!`,
//! constants `pi`/`e`, and functions `sqrt sin cos tan log ln abs floor ceil round`.

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
                    if d.is_ascii_digit() || d == '.' || d == 'e' || d == 'E' {
                        // handle scientific notation sign after e/E
                        if (d == 'e' || d == 'E')
                            && i + 1 < bytes.len()
                            && matches!(bytes[i + 1] as char, '+' | '-')
                        {
                            i += 2;
                            continue;
                        }
                        i += 1;
                    } else {
                        break;
                    }
                }
                let num: f64 = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
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
                let id = std::str::from_utf8(&bytes[start..i]).ok()?.to_ascii_lowercase();
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
        left = if *op == '+' { left + right } else { left - right };
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
}
