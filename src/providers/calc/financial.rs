//! Financial calculators: interest, discount, split, GST, EMI, CAGR,
//! rule-of-72, percent change, hourly↔annual.
//!
//! Queries: `interest 1000 at 5% for 3 years`, `20% off 500`, `split 45 4`,
//! `gst 18% on 1000`, `emi 500000 8% 5 years`, `cagr 10000 to 20000 3 years`,
//! `72 at 8%`, `100 to 150`, `25/hr to annual`, `60000/yr to hourly`.

use super::util::{card_result, format_number};
use crate::providers::SearchResult;
use once_cell::sync::Lazy;
use regex::Regex;

/// Numeric amount, optionally with a magnitude suffix (`5 lakh`, `2k`, `1.5m`
/// excluded — single letters are units). Fed to `expr::eval_str` so lakh/crore
/// and friends resolve.
const AMT: &str = r"[+-]?\d+(?:\.\d+)?(?:\s*(?:k|mil|bn|tn|thousand|thousands|million|millions|billion|billions|trillion|trillions|hundred|hundreds|lakh|lac|lakhs|lacs|l|crore|crores|cr|crs))?";

fn amt(s: &str) -> Option<f64> {
    super::expr::eval_str(s)
}

fn inr(v: f64) -> String {
    format!("₹{}", format_number(v))
}

fn fmt_pct(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}%", v.round() as i64)
    } else {
        format!("{v:.2}%")
    }
}

fn interest(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(
            r"(?i)^interest\s+({AMT})\s+at\s+(\d+(?:\.\d+)?)\s*%\s*(?:compounded?\s+)?(?:for|over)\s+(\d+(?:\.\d+)?)\s+(years?|yrs?|y|months?|mos?|mo)(?:\s+compounded?\s*(?:annually|yearly)?)?\s*$"
        ))
        .unwrap()
    });
    let c = RE.captures(q)?;
    let p = amt(c.get(1)?.as_str())?;
    let rate = c.get(2)?.as_str().parse::<f64>().ok()?;
    let t: f64 = c.get(3)?.as_str().parse().ok()?;
    let unit = c.get(4)?.as_str().to_ascii_lowercase();
    let t_years = if unit.starts_with('m') { t / 12.0 } else { t };
    if !rate.is_finite() || !t_years.is_finite() || rate <= 0.0 || t_years < 0.0 {
        return None;
    }
    let compound = q.to_ascii_lowercase().contains("compound");
    let total = if compound {
        p * (1.0 + rate / 100.0).powf(t_years)
    } else {
        p * (1.0 + rate / 100.0 * t_years)
    };
    if !total.is_finite() {
        return None;
    }
    let interest_amt = total - p;
    let shown = q.trim();
    let kind: &'static str = if compound {
        "compound interest"
    } else {
        "interest"
    };
    let copy = format!(
        "Total: {}\nPrincipal: {}\n{} earned: {} @ {}% for {} {}",
        format_number(total),
        format_number(p),
        kind,
        format_number(interest_amt),
        rate,
        t,
        unit
    );
    Some(card_result(
        format_number(total),
        format!(
            "Principal {} · {} {} earned",
            format_number(p),
            format_number(interest_amt),
            kind
        ),
        copy,
        shown.into(),
        kind,
        format_number(total),
        "total",
    ))
}

fn discount(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(r"(?i)^(\d+(?:\.\d+)?)\s*%\s*off\s+({AMT})\s*$")).unwrap()
    });
    let c = RE.captures(q)?;
    let pct: f64 = c.get(1)?.as_str().parse().ok()?;
    let base = amt(c.get(2)?.as_str())?;
    if pct <= 0.0 {
        return None;
    }
    let saved = base * pct / 100.0;
    let total = base - saved;
    let shown = q.trim();
    Some(card_result(
        format_number(total),
        format!("Save {}", format_number(saved)),
        format!(
            "{} ({} off {})",
            format_number(total),
            format_number(pct),
            format_number(base)
        ),
        shown.into(),
        "discount",
        format_number(total),
        "result",
    ))
}

fn split(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(&format!(r"(?i)^split\s+({AMT})\s+(\d+)\s*$")).unwrap());
    let c = RE.captures(q)?;
    let total = amt(c.get(1)?.as_str())?;
    let n: i64 = c.get(2)?.as_str().parse().ok()?;
    if n <= 0 {
        return None;
    }
    let per = total / n as f64;
    let shown = q.trim();
    Some(card_result(
        format!("{} each", format_number(per)),
        format!("{} ÷ {}", format_number(total), n),
        format!(
            "{} ÷ {} = {} per person",
            format_number(total),
            n,
            format_number(per)
        ),
        shown.into(),
        "split",
        format!("{} each", format_number(per)),
        "per person",
    ))
}

fn gst(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(
            r"(?i)^(?:gst\s+(\d+(?:\.\d+)?)\s*%\s+on\s+|(\d+(?:\.\d+)?)\s*%\s+gst\s+on\s+)({AMT})\s*$"
        ))
        .unwrap()
    });
    let c = RE.captures(q)?;
    let pct: f64 = c
        .get(1)
        .and_then(|m| m.as_str().parse().ok())
        .or_else(|| c.get(2).and_then(|m| m.as_str().parse().ok()))?;
    let base = amt(c.get(3)?.as_str())?;
    if pct <= 0.0 {
        return None;
    }
    let gst_amt = base * pct / 100.0;
    let total = base + gst_amt;
    let shown = q.trim();
    Some(card_result(
        inr(total),
        format!("GST {} · base {}", inr(gst_amt), inr(base)),
        format!(
            "Base: {}\nGST {}%: {}\nTotal: {}",
            inr(base),
            pct,
            inr(gst_amt),
            inr(total)
        ),
        shown.into(),
        "gst",
        inr(total),
        "total",
    ))
}

fn emi(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(
            r"(?i)^emi\s+({AMT})\s+(\d+(?:\.\d+)?)\s*%\s+(\d+(?:\.\d+)?)\s+(years?|yrs?|y|months?|mos?|mo)\s*$"
        ))
        .unwrap()
    });
    let c = RE.captures(q)?;
    let p = amt(c.get(1)?.as_str())?;
    let annual_rate: f64 = c.get(2)?.as_str().parse().ok()?;
    let t: f64 = c.get(3)?.as_str().parse().ok()?;
    let unit = c.get(4)?.as_str().to_ascii_lowercase();
    let months = if unit.starts_with('m') { t } else { t * 12.0 };
    if p <= 0.0 || annual_rate <= 0.0 || months <= 0.0 {
        return None;
    }
    let i = annual_rate / 100.0 / 12.0;
    let factor = (1.0 + i).powf(months);
    if !factor.is_finite() || factor <= 1.0 {
        return None;
    }
    let emi_amt = p * i * factor / (factor - 1.0);
    let total_payable = emi_amt * months;
    let shown = q.trim();
    Some(card_result(
        format!("{}/mo", inr(emi_amt)),
        format!(
            "P {} @ {}%/yr · {} months",
            inr(p),
            annual_rate,
            months as i64
        ),
        format!(
            "EMI: {}/mo\nPrincipal: {}\nTenure: {} months\nTotal payable: {}\nInterest: {}",
            inr(emi_amt),
            inr(p),
            months as i64,
            inr(total_payable),
            inr(total_payable - p)
        ),
        shown.into(),
        "emi",
        format!("{}/mo", inr(emi_amt)),
        "monthly",
    ))
}

fn cagr(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(
            r"(?i)^cagr\s+({AMT})\s+to\s+({AMT})\s+(\d+(?:\.\d+)?)\s+(years?|yrs?|y)\s*$"
        ))
        .unwrap()
    });
    let c = RE.captures(q)?;
    let start = amt(c.get(1)?.as_str())?;
    let end = amt(c.get(2)?.as_str())?;
    let t: f64 = c.get(3)?.as_str().parse().ok()?;
    if start <= 0.0 || end <= 0.0 || t <= 0.0 {
        return None;
    }
    let rate = (end / start).powf(1.0 / t) - 1.0;
    let pct = fmt_pct(rate * 100.0);
    let shown = q.trim();
    Some(card_result(
        pct.clone(),
        format!(
            "{} → {} over {} years",
            format_number(start),
            format_number(end),
            t
        ),
        format!(
            "CAGR {} ({} → {} over {} years)",
            pct,
            format_number(start),
            format_number(end),
            t
        ),
        shown.into(),
        "cagr",
        pct,
        "rate",
    ))
}

fn rule72(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^(?:(?:rule of )?72)\s+at\s+(\d+(?:\.\d+)?)\s*%\s*$").unwrap()
    });
    let c = RE.captures(q)?;
    let rate: f64 = c.get(1)?.as_str().parse().ok()?;
    if rate <= 0.0 {
        return None;
    }
    let years = 72.0 / rate;
    let shown = q.trim();
    Some(card_result(
        format!("{} years", format_number(years)),
        format!("72 ÷ {} = {} years to double", rate, format_number(years)),
        format!("{} years to double at {}%", format_number(years), rate),
        shown.into(),
        "rule 72",
        format!("{} years", format_number(years)),
        "result",
    ))
}

fn pct_change(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(&format!(r"(?i)^({AMT})\s+to\s+({AMT})\s*$")).unwrap());
    let c = RE.captures(q)?;
    let from = amt(c.get(1)?.as_str())?;
    let to = amt(c.get(2)?.as_str())?;
    if from == 0.0 {
        return None;
    }
    let change = (to - from) / from * 100.0;
    let sign = if change >= 0.0 { "+" } else { "" };
    let shown = q.trim();
    let pct = format!("{sign}{}", fmt_pct(change));
    Some(card_result(
        pct.clone(),
        format!("from {} to {}", format_number(from), format_number(to)),
        format!("{} ({} → {})", pct, format_number(from), format_number(to)),
        shown.into(),
        "change",
        pct,
        "percent",
    ))
}

const WORK_HOURS_PER_YEAR: f64 = 2080.0;

fn hourly_to_annual(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(
            r"(?i)^({AMT})\s*/hr\s+to\s+(annual|annum|yearly|per\s+year)\s*$"
        ))
        .unwrap()
    });
    let c = RE.captures(q)?;
    let hourly = amt(c.get(1)?.as_str())?;
    let annual = hourly * WORK_HOURS_PER_YEAR;
    let shown = q.trim();
    Some(card_result(
        format!("{}/yr", format_number(annual)),
        format!(
            "{} × {}h = {}/yr",
            format_number(hourly),
            WORK_HOURS_PER_YEAR,
            format_number(annual)
        ),
        format!(
            "{} per hour × {} hours = {} per year",
            format_number(hourly),
            WORK_HOURS_PER_YEAR,
            format_number(annual)
        ),
        shown.into(),
        "rate",
        format!("{}/yr", format_number(annual)),
        "annual",
    ))
}

fn annual_to_hourly(q: &str) -> Option<SearchResult> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(
            r"(?i)^({AMT})\s*/(?:yr|year|annum)\s+to\s+(?:hourly|per\s+hour|/hr)\s*$"
        ))
        .unwrap()
    });
    let c = RE.captures(q)?;
    let annual = amt(c.get(1)?.as_str())?;
    let hourly = annual / WORK_HOURS_PER_YEAR;
    let shown = q.trim();
    Some(card_result(
        format!("{}/hr", format_number(hourly)),
        format!(
            "{} ÷ {}h = {}/hr",
            format_number(annual),
            WORK_HOURS_PER_YEAR,
            format_number(hourly)
        ),
        format!(
            "{} per year ÷ {} hours = {} per hour",
            format_number(annual),
            WORK_HOURS_PER_YEAR,
            format_number(hourly)
        ),
        shown.into(),
        "annual",
        format!("{}/hr", format_number(hourly)),
        "hourly",
    ))
}

pub(crate) fn try_financial(q: &str) -> Option<SearchResult> {
    if let Some(r) = interest(q) {
        return Some(r);
    }
    if let Some(r) = cagr(q) {
        return Some(r);
    }
    if let Some(r) = emi(q) {
        return Some(r);
    }
    if let Some(r) = discount(q) {
        return Some(r);
    }
    if let Some(r) = gst(q) {
        return Some(r);
    }
    if let Some(r) = split(q) {
        return Some(r);
    }
    if let Some(r) = rule72(q) {
        return Some(r);
    }
    if let Some(r) = pct_change(q) {
        return Some(r);
    }
    if let Some(r) = hourly_to_annual(q) {
        return Some(r);
    }
    annual_to_hourly(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_badges(r: &SearchResult) -> (String, String) {
        let c = r.conversion.as_ref().expect("card");
        (c.left_badge.clone(), c.right_badge.clone())
    }

    #[test]
    fn simple_interest() {
        let r = try_financial("interest 1000 at 5% for 3 years").expect("interest");
        assert_eq!(r.title, "1150");
        assert_eq!(r.subtitle, "Principal 1000 · 150 interest earned");
        assert_eq!(card_badges(&r), ("interest".into(), "total".into()));
        // Months roll to fractional years.
        let r = try_financial("interest 1000 at 12% for 6 months").expect("interest");
        assert_eq!(r.title, "1060");
        // Magnitude principal works via eval_str.
        let r = try_financial("interest 5 lakh at 10% for 2 years").expect("interest");
        assert_eq!(r.title, "600000");
        // Short forms: `1 cr` = 1 crore.
        let r = try_financial("interest 1 cr at 10% for 1 year").expect("crore short");
        assert_eq!(r.title, "11000000");
        // Compound (annual) beats simple.
        let r = try_financial("interest 1000 at 5% compounded for 3 years").expect("compound");
        assert_eq!(r.title, "1157.63");
        let r =
            try_financial("interest 1000 at 5% for 3 years compounded annually").expect("compound");
        assert_eq!(r.title, "1157.63");
    }

    #[test]
    fn interest_rejects_non_finite_results() {
        let huge_years = format!("1{}", "0".repeat(308));
        let q = format!("interest 1 crore at 5% for {huge_years} years compounded");
        assert!(try_financial(&q).is_none());
    }

    #[test]
    fn discount() {
        let r = try_financial("20% off 500").expect("discount");
        assert_eq!(r.title, "400");
        assert_eq!(r.subtitle, "Save 100");
        assert_eq!(card_badges(&r), ("discount".into(), "result".into()));
        // Must not steal "N% of M".
        assert!(try_financial("50% of 100").is_none());
    }

    #[test]
    fn bill_split() {
        let r = try_financial("split 45 4").expect("split");
        assert_eq!(r.title, "11.25 each");
        assert_eq!(card_badges(&r), ("split".into(), "per person".into()));
        assert!(try_financial("split 45 0").is_none());
    }

    #[test]
    fn gst() {
        let r = try_financial("gst 18% on 1000").expect("gst");
        assert_eq!(r.title, "₹1180");
        assert_eq!(r.subtitle, "GST ₹180 · base ₹1000");
        assert_eq!(card_badges(&r), ("gst".into(), "total".into()));
        let r = try_financial("18% gst on 1000").expect("gst alt order");
        assert_eq!(r.title, "₹1180");
    }

    #[test]
    fn emi() {
        let r = try_financial("emi 500000 8% 5 years").expect("emi");
        assert_eq!(r.title, "₹10138.2/mo");
        assert_eq!(card_badges(&r), ("emi".into(), "monthly".into()));
    }

    #[test]
    fn cagr() {
        let r = try_financial("cagr 10000 to 20000 3 years").expect("cagr");
        assert_eq!(r.title, "25.99%");
        assert_eq!(card_badges(&r), ("cagr".into(), "rate".into()));
    }

    #[test]
    fn rule_72() {
        let r = try_financial("72 at 8%").expect("rule72");
        assert_eq!(r.title, "9 years");
        let r = try_financial("rule of 72 at 8%").expect("rule72 long");
        assert_eq!(r.title, "9 years");
    }

    #[test]
    fn percent_change() {
        let r = try_financial("100 to 150").expect("pct change");
        assert_eq!(r.title, "+50%");
        assert_eq!(card_badges(&r), ("change".into(), "percent".into()));
        let r = try_financial("150 to 100").expect("pct change down");
        assert_eq!(r.title, "-33.33%");
        // Unit conversion must not be swallowed.
        assert!(try_financial("5 km to miles").is_none());
        assert!(try_financial("100 usd to eur").is_none());
    }

    #[test]
    fn hourly_annual() {
        let r = try_financial("25/hr to annual").expect("hourly→annual");
        assert_eq!(r.title, "52000/yr");
        assert_eq!(card_badges(&r), ("rate".into(), "annual".into()));
        let r = try_financial("60000/yr to hourly").expect("annual→hourly");
        assert_eq!(r.title, "28.8462/hr");
        assert_eq!(card_badges(&r), ("annual".into(), "hourly".into()));
    }

    #[test]
    fn cards_carry_conversion() {
        for q in [
            "interest 1000 at 5% for 3 years",
            "20% off 500",
            "split 45 4",
            "gst 18% on 1000",
            "emi 500000 8% 5 years",
            "cagr 10000 to 20000 3 years",
            "72 at 8%",
            "100 to 150",
            "25/hr to annual",
            "60000/yr to hourly",
        ] {
            let r = try_financial(q).expect(q);
            assert!(r.conversion.is_some(), "{q}");
        }
    }
}
