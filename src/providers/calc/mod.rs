mod currency;
mod datetime;
mod duration;
mod math;
mod timezone;
mod units;
mod util;

use currency::{normalize_money_query, try_currency, try_currency_predict};
use datetime::try_datetime;
use duration::try_duration_expr;
use math::{looks_like_math, try_math, try_natural};
use timezone::{try_timezone, try_timezone_predict};
use units::{try_conversion, try_conversion_predict};

use super::fx::FxStore;
use super::{Provider, SearchResult};
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

    #[allow(dead_code)] // was used for boot-time warm; FX now lazy on convert
    pub fn fx_store(&self) -> Arc<FxStore> {
        self.fx.clone()
    }
}

impl Provider for CalcProvider {
    fn search(&self, query: &str) -> Vec<SearchResult> {
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
        if let Some(r) = try_conversion(&q_norm) {
            return vec![r];
        }
        if let Some(results) = try_conversion_predict(&q_norm) {
            return results;
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
            | "settings"
            | "preferences"
            | "index"
            | "config"
    ) {
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
    if q.chars().any(|c| matches!(c, '$' | '€' | '£' | '¥' | '₹' | '₩' | '₽')) {
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
