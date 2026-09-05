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
fn looks_like_plain_text(q: &str) -> bool {
    let lower = q.to_ascii_lowercase();
    // Keep natural/datetime keywords
    if matches!(
        lower.as_str(),
        "now"
            | "time"
            | "date"
            | "today"
            | "tomorrow"
            | "yesterday"
            | "utc"
            | "now utc"
            | "unix"
            | "epoch"
            | "to unix"
            | "unix now"
            | "week"
            | "week number"
            | "iso week"
            | "day of year"
            | "doy"
            | "settings"
            | "preferences"
            | "index"
            | "config"
    ) {
        return false;
    }
    if battery::is_battery_keyword(&lower) {
        return false;
    }
    // Quickwin commands that are pure letters (no digits to trigger math).
    if lower.starts_with("dice")
        || lower.starts_with("coin")
        || lower.starts_with("roll ")
        || lower.starts_with("random")
        || lower.starts_with("uuid")
        || lower.starts_with("password")
        || lower.starts_with("wc ")
        || lower.starts_with("slug ")
        || lower.starts_with("case ")
        || lower.starts_with("roman ")
    {
        return false;
    }

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
    // Math function names
    if lower.contains("sqrt")
        || lower.contains("sin")
        || lower.contains("cos")
        || lower.contains("tan")
        || lower.contains("log")
        || lower.contains("pi")
    {
        return false;
    }

    // Otherwise pure letters/spaces/punctuation → app/file query
    true
}
