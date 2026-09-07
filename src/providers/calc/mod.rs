mod battery;
mod cooking;
pub(crate) mod currency;
mod datetime;
mod duration;
mod expr;
mod financial;
mod fueleco;
mod home;
mod math;
mod quick;
mod timezone;
mod unitmath;
mod units;
mod util;

use battery::try_battery;
use cooking::{try_cooking, try_oven, try_recipe_scale};
use currency::{normalize_money_query, try_currency, try_currency_home, try_currency_predict};
use datetime::try_datetime;
use duration::try_duration_expr;
use financial::try_financial;
use fueleco::try_fuel_economy;
use math::{looks_like_math, try_math, try_natural};
use quick::try_quickwin;
use timezone::{try_timezone, try_timezone_predict};
use unitmath::try_unit_math;
use units::{try_conversion, try_conversion_predict, try_unit_home};

use super::fx::FxStore;
use super::SearchResult;
use std::sync::Arc;

pub struct CalcProvider {
    fx: Arc<FxStore>,
}

impl CalcProvider {
    pub fn new() -> Self {
        Self {
            fx: Arc::new(FxStore::new()),
        }
    }

    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }

        // Fast reject pure app/file-like text before expensive regex stack.
        // Keep words that datetime/natural still need ("now", "today", …).
        if looks_like_plain_text(q) {
            return Vec::new();
        }

        let q_norm = normalize_money_query(q);

        if let Some(r) = try_battery(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_duration_expr(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_timezone(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_timezone_predict(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_currency(&q_norm, &self.fx) {
            return vec![r];
        }
        if let Some(r) = try_currency_predict(&q_norm, &self.fx) {
            return vec![r];
        }
        // Bare `10usd` (no target) → home currency.
        if let Some(r) = try_currency_home(&q_norm, &self.fx) {
            return vec![r];
        }
        if let Some(r) = try_recipe_scale(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_cooking(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_oven(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_conversion(&q_norm) {
            return vec![r];
        }
        if let Some(results) = try_conversion_predict(&q_norm) {
            return results;
        }
        // Bare `10miles` (no target) → home default for the category.
        if let Some(r) = try_unit_home(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_quickwin(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_financial(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_fuel_economy(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_unit_math(&q_norm) {
            return vec![r];
        }
        if let Some(r) = try_datetime(&q_norm) {
            return vec![r];
        }
        if looks_like_math(&q_norm) {
            if let Some(r) = try_math(&q_norm) {
                return vec![r];
            }
        }
        if let Some(r) = try_natural(&q_norm) {
            return vec![r];
        }
        Vec::new()
    }
}

impl Default for CalcProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// True when query is almost certainly not calc/convert/timezone.
///
/// Allocation-free: fixed keywords use `eq_ignore_ascii_case`, prefixes use
/// boundary-safe slices, function names use byte windows. Runs per keystroke
/// for every query, so no `to_lowercase()` here. Fast digit/operator checks
/// come first so math-like queries exit before any string scans.
fn looks_like_plain_text(q: &str) -> bool {
    // Digits → may be math/units/currency/time
    if q.bytes().any(|b| b.is_ascii_digit()) {
        return false;
    }
    // Operators / conversion markers
    if q.contains('+')
        || q.contains('*')
        || q.contains('/')
        || q.contains('%')
        || q.contains('^')
        || q.contains('=')
        || q.contains('→')
        || q.contains(" to ")
        || q.contains(" in ")
        || q.contains(" as ")
        || q.contains("->")
    {
        return false;
    }
    // Currency symbols
    if q.chars()
        .any(|c| matches!(c, '$' | '€' | '£' | '¥' | '₹' | '₩' | '₽'))
    {
        return false;
    }
    // Keep natural/datetime keywords
    if q.eq_ignore_ascii_case("now")
        || q.eq_ignore_ascii_case("time")
        || q.eq_ignore_ascii_case("date")
        || q.eq_ignore_ascii_case("today")
        || q.eq_ignore_ascii_case("tomorrow")
        || q.eq_ignore_ascii_case("yesterday")
        || q.eq_ignore_ascii_case("utc")
        || q.eq_ignore_ascii_case("now utc")
        || q.eq_ignore_ascii_case("unix")
        || q.eq_ignore_ascii_case("epoch")
        || q.eq_ignore_ascii_case("to unix")
        || q.eq_ignore_ascii_case("unix now")
        || q.eq_ignore_ascii_case("week")
        || q.eq_ignore_ascii_case("week number")
        || q.eq_ignore_ascii_case("iso week")
        || q.eq_ignore_ascii_case("day of year")
        || q.eq_ignore_ascii_case("doy")
        || q.eq_ignore_ascii_case("settings")
        || q.eq_ignore_ascii_case("preferences")
        || q.eq_ignore_ascii_case("index")
        || q.eq_ignore_ascii_case("config")
    {
        return false;
    }
    if battery::is_battery_keyword(q) {
        return false;
    }
    // Quickwin commands that are pure letters (no digits to trigger math).
    // Boundary-safe prefix slices (`get` returns None inside multi-byte chars).
    for prefix in [
        "dice", "coin", "roll ", "random", "uuid", "password", "wc ", "slug ", "case ",
        "roman ",
    ] {
        if q
            .get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
        {
            return false;
        }
    }

    // Math function names (case-insensitive, no alloc)
    if util::contains_ignore_ascii_case(q, "sqrt")
        || util::contains_ignore_ascii_case(q, "sin")
        || util::contains_ignore_ascii_case(q, "cos")
        || util::contains_ignore_ascii_case(q, "tan")
        || util::contains_ignore_ascii_case(q, "log")
        || util::contains_ignore_ascii_case(q, "pi")
    {
        return false;
    }

    // Otherwise pure letters/spaces/punctuation → app/file query
    true
}
