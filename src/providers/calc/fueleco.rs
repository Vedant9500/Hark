//! Fuel economy conversions: `12 km/l to mpg`, `30 mpg to l/100km`.
//!
//! Closes the T1 "compound units — fuel economy" gap (`5km/2h` speed already
//! lives in `unitmath`). US mpg; `mpg` defaults to US gallons (3.785411784 L),
//! which is what `km/l → mpg` normally means.

use super::util::{card_result, format_number};
use crate::providers::SearchResult;

const KM_PER_MILE: f64 = 1.609_344;
const L_PER_US_GAL: f64 = 3.785_411_784;
const KML_TO_MPG: f64 = L_PER_US_GAL / KM_PER_MILE; // ≈ 2.35215
const L100_PER_MPG: f64 = 100.0 * L_PER_US_GAL / KM_PER_MILE; // ≈ 235.215

#[derive(Clone, Copy, PartialEq, Eq)]
enum FuelUnit {
    Kml,
    Mpg,
    L100,
}

impl FuelUnit {
    fn label(self) -> &'static str {
        match self {
            FuelUnit::Kml => "km/l",
            FuelUnit::Mpg => "mpg",
            FuelUnit::L100 => "l/100km",
        }
    }
}

/// Parse a unit token, normalizing " per " to "/" and stripping spaces so word
/// forms (`km per litre`, `miles per gallon`, `l per 100 km`) all collapse.
fn fuel_unit(s: &str) -> Option<FuelUnit> {
    let t: String = s
        .trim()
        .to_ascii_lowercase()
        .replace(" per ", "/")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    match t.as_str() {
        "km/l" | "kmpl" | "km/litre" | "km/liter" | "kilometre/litre" | "kilometer/liter" => {
            Some(FuelUnit::Kml)
        }
        "mpg" | "miles/gallon" => Some(FuelUnit::Mpg),
        "l/100km" | "l/100" | "l/100kilometre" | "l/100kilometer" | "litre/100km"
        | "liter/100km" => Some(FuelUnit::L100),
        _ => None,
    }
}

/// `N <from-unit> to <to-unit>`. Left and right split on ` to ` (case-folded);
/// units may be slash or word forms.
fn convert(q: &str) -> Option<f64> {
    let lower = q.to_ascii_lowercase();
    let (lhs, rhs) = lower.split_once(" to ")?;
    let mut parts = lhs.trim().splitn(2, char::is_whitespace);
    let value: f64 = parts.next()?.parse().ok()?;
    let from = fuel_unit(parts.next()?)?;
    let to = fuel_unit(rhs.trim())?;
    if !value.is_finite() || value <= 0.0 || from == to {
        return None;
    }
    let out = match (from, to) {
        (FuelUnit::Kml, FuelUnit::Mpg) => value * KML_TO_MPG,
        (FuelUnit::Kml, FuelUnit::L100) => 100.0 / value,
        (FuelUnit::Mpg, FuelUnit::Kml) => value / KML_TO_MPG,
        (FuelUnit::Mpg, FuelUnit::L100) => L100_PER_MPG / value,
        (FuelUnit::L100, FuelUnit::Kml) => 100.0 / value,
        (FuelUnit::L100, FuelUnit::Mpg) => L100_PER_MPG / value,
        _ => return None,
    };
    out.is_finite().then_some(out)
}

pub(crate) fn try_fuel_economy(q: &str) -> Option<SearchResult> {
    let lower = q.to_ascii_lowercase();
    if !lower.contains(" to ") {
        return None;
    }
    let out = convert(q)?;
    let to_label = out_label(q)?;
    let title = format!("{} {}", format_number(out), to_label);
    let shown = q.trim().to_string();
    Some(card_result(
        title.clone(),
        format!("{} → {}", format_number(out), to_label),
        format!("{shown} = {}", format_number(out)),
        shown,
        "fuel economy",
        title,
        to_label,
    ))
}

fn out_label(q: &str) -> Option<&'static str> {
    let lower = q.to_ascii_lowercase();
    let (_, rhs) = lower.split_once(" to ")?;
    fuel_unit(rhs.trim()).map(FuelUnit::label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kml_to_mpg() {
        assert_eq!(
            try_fuel_economy("12 km/l to mpg").expect("kml→mpg").title,
            "28.2258 mpg"
        );
        let r = try_fuel_economy("12 km per litre to mpg").expect("word form");
        assert_eq!(r.title, "28.2258 mpg");
        assert_eq!(r.conversion.as_ref().unwrap().left_badge, "fuel economy");
        assert_eq!(r.conversion.as_ref().unwrap().right_badge, "mpg");
    }

    #[test]
    fn mpg_to_l100km() {
        assert_eq!(
            try_fuel_economy("30 mpg to l/100km")
                .expect("mpg→l100")
                .title,
            "7.8405 l/100km"
        );
        assert_eq!(
            try_fuel_economy("30 mpg to l/100 km")
                .expect("spaced")
                .title,
            "7.8405 l/100km"
        );
    }

    #[test]
    fn other_directions() {
        assert_eq!(
            try_fuel_economy("30 mpg to km/l").expect("mpg→kml").title,
            "12.7543 km/l"
        );
        assert_eq!(
            try_fuel_economy("12 km/l to l/100km")
                .expect("kml→l100")
                .title,
            "8.3333 l/100km"
        );
        assert_eq!(
            try_fuel_economy("7.84 l/100km to mpg")
                .expect("l100→mpg")
                .title,
            "30.0019 mpg"
        );
        assert_eq!(
            try_fuel_economy("7.84 l/100km to km/l")
                .expect("l100→kml")
                .title,
            "12.7551 km/l"
        );
    }

    #[test]
    fn rejects_non_fuel() {
        assert!(try_fuel_economy("5 km to miles").is_none());
        assert!(try_fuel_economy("12 km/l to km/l").is_none());
        assert!(try_fuel_economy("12 to 30").is_none());
        assert!(try_fuel_economy("firefox").is_none());
    }

    #[test]
    fn rejects_non_finite_values() {
        assert!(try_fuel_economy("NaN mpg to l/100km").is_none());
        assert!(try_fuel_economy("inf mpg to l/100km").is_none());
        assert!(try_fuel_economy("1e309 mpg to l/100km").is_none());
    }
}
