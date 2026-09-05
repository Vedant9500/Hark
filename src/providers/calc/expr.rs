//! Small expression evaluator for launcher math.
//!
//! Replaces abandoned `meval` (which pulled ancient `nom` with future-incompat lints).
//! Supports: + - * / % ** ^, unary ±, parentheses, factorial postfix `!`,
//! constants `pi`/`e`, functions `sqrt sin cos tan log ln abs floor ceil round`,
//! and magnitude suffixes (`5k`, `1.5 million`, `2 billion`, …).

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
    let v = parse_expr(&tokens, &mut i, 0)?;
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

// Pratt / recursive descent: expr → add → mul → unary → pow → primary
//
// Every `(` and unary sign recurses through the whole chain; a depth budget
// (counted at `parse_expr`) bounds stack usage — deeply nested input returns
// `None` instead of overflowing the stack (a runtime abort, not a panic).
const MAX_DEPTH: usize = 200;

fn parse_expr(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<f64> {
    if depth > MAX_DEPTH {
        return None;
    }
    parse_add(tokens, i, depth)
}

fn parse_add(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<f64> {
    let mut left = parse_mul(tokens, i, depth)?;
    while let Some(Tok::Op(op)) = tokens.get(*i) {
        if *op != '+' && *op != '-' {
            break;
        }
        *i += 1;
        let right = parse_mul(tokens, i, depth)?;
        left = if *op == '+' {
            left + right
        } else {
            left - right
        };
    }
    Some(left)
}

fn parse_mul(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<f64> {
    let mut left = parse_unary(tokens, i, depth)?;
    while let Some(Tok::Op(op)) = tokens.get(*i) {
        if *op != '*' && *op != '/' && *op != '%' {
            break;
        }
        *i += 1;
        let right = parse_unary(tokens, i, depth)?;
        left = match *op {
            '*' => left * right,
            '/' => {
                if right == 0.0 {
                    return None;
                }
                left / right
            }
            '%' => {
                if right == 0.0 {
                    return None;
                }
                left % right
            }
            _ => unreachable!(),
        };
    }
    Some(left)
}

/// Right-associative power over a primary base, then postfix factorial.
///
/// Unary minus binds looser than `^` (`-2^2` is `-(2^2)`), so the base comes
/// from `parse_primary` and the (possibly signed) exponent from `parse_unary`.
fn parse_pow(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<f64> {
    let mut left = parse_primary(tokens, i, depth)?;
    // postfix !
    while matches!(tokens.get(*i), Some(Tok::Op('!'))) {
        *i += 1;
        left = factorial(left)?;
    }
    if matches!(tokens.get(*i), Some(Tok::Op('^'))) {
        *i += 1;
        let right = parse_unary(tokens, i, depth)?; // right-assoc via unary→pow
        left = left.powf(right);
        // more postfix after power result? rare; allow !
        while matches!(tokens.get(*i), Some(Tok::Op('!'))) {
            *i += 1;
            left = factorial(left)?;
        }
    }
    Some(left)
}

fn parse_unary(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<f64> {
    // Sign chains (`----3`) recurse unary→unary without re-entering
    // parse_expr, so this arm needs its own budget check.
    if depth > MAX_DEPTH {
        return None;
    }
    match tokens.get(*i) {
        Some(Tok::Op('+')) => {
            *i += 1;
            parse_unary(tokens, i, depth + 1)
        }
        Some(Tok::Op('-')) => {
            *i += 1;
            // Recurse through unary (not pow) so doubled signs like `--3` and
            // `5--3` keep parsing; precedence is unchanged since the default
            // arm descends into pow anyway.
            Some(-parse_unary(tokens, i, depth + 1)?)
        }
        _ => parse_pow(tokens, i, depth),
    }
}

fn parse_primary(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<f64> {
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
                        args.push(parse_expr(tokens, i, depth + 1)?);
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
            let v = parse_expr(tokens, i, depth + 1)?;
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

// ---------------------------------------------------------------------------
// Precedence printer
//
// Reprints a token stream with explicit grouping (`-2^2` → `-(2^2)`) by
// walking the same grammar as the parser above. Never evaluates: numbers are
// echoed via their token value only.
// ---------------------------------------------------------------------------

/// Binding levels for printed sub-expressions; used to decide where parens
/// are required to keep re-evaluation identical to the original input.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Lvl {
    Add = 1,
    Mul = 2,
    Unary = 3,
    Pow = 4,
    Primary = 5,
}

/// Add parens iff `lvl` binds looser than the context requires (`min`).
fn wrap(s: String, lvl: Lvl, min: Lvl) -> String {
    if lvl < min {
        format!("({s})")
    } else {
        s
    }
}

/// Reprint `input` making precedence explicit, or `None` if it does not parse.
///
/// Mirrors [`eval_str`]'s guards: non-empty tokens, full consumption.
pub(crate) fn explain_str(input: &str) -> Option<String> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return None;
    }
    let mut i = 0;
    let (s, _) = print_add(&tokens, &mut i, 0)?;
    if i != tokens.len() {
        return None;
    }
    Some(s)
}

fn print_add(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<(String, Lvl)> {
    if depth > MAX_DEPTH {
        return None;
    }
    let (mut s, mut lvl) = print_mul(tokens, i, depth)?;
    while let Some(Tok::Op(op)) = tokens.get(*i) {
        if *op != '+' && *op != '-' {
            break;
        }
        *i += 1;
        let (r, _) = print_mul(tokens, i, depth)?;
        s = format!("{s} {op} {r}");
        lvl = Lvl::Add;
    }
    Some((s, lvl))
}

fn print_mul(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<(String, Lvl)> {
    let (mut s, mut lvl) = print_unary(tokens, i, depth)?;
    while let Some(Tok::Op(op)) = tokens.get(*i) {
        if *op != '*' && *op != '/' && *op != '%' {
            break;
        }
        *i += 1;
        let (r, _) = print_unary(tokens, i, depth)?;
        s = format!("{s} {op} {r}");
        lvl = Lvl::Mul;
    }
    Some((s, lvl))
}

/// Mirror of [`parse_unary`]: sign chains descend into pow; composite
/// operands are grouped (`--3` → `-(-3)`), bare atoms stay ungrouped.
fn print_unary(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<(String, Lvl)> {
    match tokens.get(*i) {
        Some(Tok::Op(sign @ ('+' | '-'))) => {
            *i += 1;
            let (inner, lvl) = print_unary(tokens, i, depth)?;
            // Composite inner already carries its own grouping level below
            // Primary, so wrap it; bare number/const/funccall stays `-3`.
            let s = if lvl < Lvl::Primary {
                format!("{sign}({inner})")
            } else {
                format!("{sign}{inner}")
            };
            Some((s, Lvl::Unary))
        }
        _ => print_pow(tokens, i, depth),
    }
}

/// Mirror of [`parse_pow`]: primary base, postfix `!`, single right-assoc
/// `^` whose exponent comes from the unary path (right-assoc via recursion).
fn print_pow(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<(String, Lvl)> {
    let (mut s, mut lvl) = print_primary(tokens, i, depth)?;
    while matches!(tokens.get(*i), Some(Tok::Op('!'))) {
        *i += 1;
        s = format!("{s}!"); // factorial is tightest; result stays atomic
        lvl = Lvl::Primary;
    }
    if matches!(tokens.get(*i), Some(Tok::Op('^'))) {
        *i += 1;
        let (exp, elvl) = print_unary(tokens, i, depth)?;
        s = format!("{s}^{}", wrap(exp, elvl, Lvl::Primary));
        lvl = Lvl::Pow;
        while matches!(tokens.get(*i), Some(Tok::Op('!'))) {
            *i += 1;
            s = format!("{s}!");
            lvl = Lvl::Primary;
        }
    }
    Some((s, lvl))
}

fn print_primary(tokens: &[Tok], i: &mut usize, depth: usize) -> Option<(String, Lvl)> {
    match tokens.get(*i).cloned() {
        Some(Tok::Num(n)) => {
            *i += 1;
            Some((format!("{n}"), Lvl::Primary))
        }
        Some(Tok::Ident(name)) => {
            *i += 1;
            if matches!(tokens.get(*i), Some(Tok::Op('('))) {
                *i += 1;
                let mut args = Vec::new();
                if !matches!(tokens.get(*i), Some(Tok::Op(')'))) {
                    loop {
                        let (arg, _) = print_add(tokens, i, depth + 1)?;
                        args.push(arg);
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
                Some((format!("{name}({})", args.join(", ")), Lvl::Primary))
            } else {
                // Constant (`pi`, `e`) — echo as written.
                Some((name, Lvl::Primary))
            }
        }
        Some(Tok::Op('(')) => {
            *i += 1;
            let (s, _) = print_add(tokens, i, depth + 1)?;
            if !matches!(tokens.get(*i), Some(Tok::Op(')'))) {
                return None;
            }
            *i += 1;
            // Literal parens make this atomic at any outer position.
            Some((format!("({s})"), Lvl::Primary))
        }
        _ => None,
    }
}

/// Scale factor for finance-style magnitude words/letters after a number.
///
/// Short letters: `k` (and multi-letter `mil`/`bn`/`tn`). Full words:
/// thousand…trillion. Single `m`/`b`/`t` are deliberately NOT magnitudes —
/// they collide with meters/bytes/tonnes (`100m` is 100 meters, not 100
/// million). Unknown words return `None` so unit tokens like `km` are left
/// alone.
fn magnitude_factor(word: &str) -> Option<f64> {
    match word {
        "k" | "thousand" | "thousands" => Some(1_000.0),
        "mil" | "million" | "millions" => Some(1_000_000.0),
        "bn" | "billion" | "billions" => Some(1_000_000_000.0),
        "tn" | "trillion" | "trillions" => Some(1_000_000_000_000.0),
        "hundred" | "hundreds" => Some(100.0),
        // Common South-Asian scales (optional nicety).
        "lakh" | "lac" | "lakhs" | "lacs" | "l" => Some(100_000.0),
        "crore" | "crores" | "cr" | "crs" => Some(10_000_000.0),
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
    if !(0.0..=170.0).contains(&x) {
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
    fn unary_minus_binds_looser_than_pow() {
        // Conventional precedence: `^` before unary `-`, factorial tightest.
        assert!((eval_str("-2^2").unwrap() + 4.0).abs() < 1e-12); // -(2^2)
        assert!((eval_str("2^-3").unwrap() - 0.125).abs() < 1e-12);
        assert!((eval_str("2^3^2").unwrap() - 512.0).abs() < 1e-9); // 2^(3^2), right-assoc
        assert!((eval_str("-3!").unwrap() + 6.0).abs() < 1e-12); // -(3!)
        assert!((eval_str("(-2)^2").unwrap() - 4.0).abs() < 1e-12);
    }

    #[test]
    fn explain_shows_interpreted_grouping() {
        assert_eq!(explain_str("-2^2").as_deref(), Some("-(2^2)"));
        assert_eq!(explain_str("2^-3").as_deref(), Some("2^(-3)"));
        assert_eq!(explain_str("2^3^2").as_deref(), Some("2^(3^2)"));
        // Composite operands group; atoms and calls stay bare.
        assert_eq!(explain_str("-sqrt(16)").as_deref(), Some("-sqrt(16)"));
        assert_eq!(explain_str("(1+2)*3").as_deref(), Some("(1 + 2) * 3"));
        assert_eq!(explain_str("5--3").as_deref(), Some("5 - -3"));
        assert_eq!(explain_str("-3!").as_deref(), Some("-3!"));
        // Re-explaining an explained expression is a no-op.
        let once = explain_str("-2^3^2").unwrap();
        assert_eq!(explain_str(&once).as_deref(), Some(once.as_str()));
    }

    #[test]
    fn explain_never_changes_semantics() {
        let cases = [
            "-2^2",
            "2^-3",
            "2^3^2",
            "-sqrt(16)",
            "(1+2)*3",
            "5--3",
            "-3!",
            "0.5*4+1",
            "2**10",
            "10%3",
            "--3",
            "-(2+3)^2",
            "abs(-7)+floor(2.7)",
            "max(3,9)-min(1,2)",
            "pi*2+e",
            "(2^2)!",
        ];
        for e in cases {
            let orig = eval_str(e).unwrap_or_else(|| panic!("case {e} must eval"));
            let printed = explain_str(e).unwrap_or_else(|| panic!("case {e} must explain"));
            let again = eval_str(&printed).unwrap_or_else(|| panic!("reprint {printed} must eval"));
            assert!(
                (orig - again).abs() < 1e-9,
                "{e}: {orig} != reprint {printed} -> {again}"
            );
        }
    }

    #[test]
    fn doubled_signs_still_parse() {
        assert!((eval_str("--3").unwrap() - 3.0).abs() < 1e-12);
        assert!((eval_str("5--3").unwrap() - 8.0).abs() < 1e-12);
        assert!((eval_str("2---3").unwrap() + 1.0).abs() < 1e-12);
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
        assert!(eval_str("5%0").is_none());
        assert!(eval_str("5 % 0").is_none());
        assert!(eval_str("nope").is_none());
    }

    #[test]
    fn deep_nesting_bounded_not_stack_overflow() {
        // Audit P1: 15k nested parens used to abort the daemon with a stack
        // overflow. The depth budget turns it into a clean `None`.
        let deep = format!("{}1+1{}", "(".repeat(15_000), ")".repeat(15_000));
        assert!(eval_str(&deep).is_none());
        assert!(explain_str(&deep).is_none());
        // A long sign chain hits the same recursion cycle via parse_unary.
        let signs = format!("-{}", "-".repeat(15_000));
        assert!(eval_str(&signs).is_none());
        // Shallow nesting still parses and evaluates normally.
        let ok = format!("{}1+1{}", "(".repeat(50), ")".repeat(50));
        assert!((eval_str(&ok).unwrap() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn magnitude_suffixes() {
        assert!((eval_str("5k").unwrap() - 5_000.0).abs() < 1e-9);
        assert!((eval_str("1.5k").unwrap() - 1_500.0).abs() < 1e-9);
        assert!((eval_str("2K").unwrap() - 2_000.0).abs() < 1e-9);
        assert!((eval_str("3 million").unwrap() - 3_000_000.0).abs() < 1e-6);
        assert!((eval_str("1.5 million").unwrap() - 1_500_000.0).abs() < 1e-6);
        assert!((eval_str("2 billion").unwrap() - 2_000_000_000.0).abs() < 1e-3);
        assert!((eval_str("1 trillion").unwrap() - 1e12).abs() < 1.0);
        assert!((eval_str("2bn").unwrap() - 2e9).abs() < 1e-3);
        // Expressions
        assert!((eval_str("5k + 2k").unwrap() - 7_000.0).abs() < 1e-9);
        assert!((eval_str("1 million / 4").unwrap() - 250_000.0).abs() < 1e-6);
        assert!((eval_str("10k * 3").unwrap() - 30_000.0).abs() < 1e-9);
        assert!((eval_str("(2.5 million + 500k) / 1k").unwrap() - 3_000.0).abs() < 1e-6);
        // Scientific still works; not confused with magnitude
        assert!((eval_str("1e3").unwrap() - 1_000.0).abs() < 1e-12);
        assert!((eval_str("1e3 + 1k").unwrap() - 2_000.0).abs() < 1e-9);
        // South-Asian shorts: `1.5cr`, `5l`.
        assert!((eval_str("1.5cr").unwrap() - 15_000_000.0).abs() < 1.0);
        assert!((eval_str("5l").unwrap() - 500_000.0).abs() < 1e-9);
        // Unit-like tokens must not be partially eaten as magnitude
        assert!(eval_str("10km").is_none());
    }

    #[test]
    fn single_letters_mbt_are_units_not_magnitudes() {
        // `m`/`b`/`t` after a number are meters/bytes/tonnes, not
        // million/billion/trillion — the unit token aborts the pure-math parse.
        assert!(eval_str("100m").is_none());
        assert!(eval_str("100m / 2").is_none());
        assert!(eval_str("1m * 3").is_none());
        assert!(eval_str("5b").is_none());
        assert!(eval_str("1b / 2").is_none());
        assert!(eval_str("2t").is_none());
        assert!(eval_str("2t / 4").is_none());
        assert!(eval_str("2m + 500k").is_none());
        // Multi-letter or word magnitudes still work.
        assert!((eval_str("5k").unwrap() - 5_000.0).abs() < 1e-9);
        assert!((eval_str("1.5mil").unwrap() - 1_500_000.0).abs() < 1e-6);
        assert!((eval_str("2tn").unwrap() - 2e12).abs() < 1.0);
        assert!((eval_str("5bn").unwrap() - 5e9).abs() < 1e-3);
    }
}
