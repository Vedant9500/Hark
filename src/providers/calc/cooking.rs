//! Cooking tools: ingredient density (weight↔volume), butter sticks, recipe
//! scaling, oven fan↔conventional offset.
//!
//! Queries: `100g flour in cups`, `2 cups sugar in g`, `1 stick butter`,
//! `double 2 cups flour`, `scale 1.5x 200g rice`, `4 servings to 8 2 cups rice`,
//! `fan 200c to conventional`, `conventional 220 c to fan`.

use super::util::{card_result, format_number, parse_qty_number};
use crate::providers::SearchResult;
use once_cell::sync::Lazy;
use regex::Regex;

/// US customary cup in ml (the reference volume for the density table).
const ML_PER_CUP: f64 = 236.588_236_5;
/// 1 stick of butter = 113.4 g (8 tbsp, ½ cup).
const G_PER_BUTTER_STICK: f64 = 113.4;
/// Fan ovens run ≈ 20 °C cooler than conventional; add on conversion.
const FAN_OFFSET_C: f64 = 20.0;

struct Ingredient {
    name: &'static str,
    aliases: &'static [&'static str],
    /// Grams per US cup (the kitchen-standard way densities are quoted).
    g_per_cup: f64,
}

/// Static density table. Values are the common kitchen figures (grams per
/// US cup) for each ingredient.
static INGREDIENTS: &[Ingredient] = &[
    Ingredient { name: "flour", aliases: &["flour", "maida", "atta"], g_per_cup: 125.0 },
    Ingredient { name: "sugar", aliases: &["sugar", "caster", "castor"], g_per_cup: 200.0 },
    Ingredient { name: "butter", aliases: &["butter"], g_per_cup: 227.0 },
    Ingredient { name: "rice", aliases: &["rice", "basmati", "jasmine", "arborio"], g_per_cup: 185.0 },
    Ingredient { name: "oats", aliases: &["oats", "oatmeal", "rolled oats"], g_per_cup: 80.0 },
    Ingredient { name: "honey", aliases: &["honey"], g_per_cup: 340.0 },
    Ingredient { name: "milk", aliases: &["milk"], g_per_cup: 244.0 },
    Ingredient { name: "oil", aliases: &["oil", "vegetable oil", "olive oil", "canola oil"], g_per_cup: 218.0 },
];

fn find_ingredient(tail: &str) -> Option<&'static Ingredient> {
    let t = tail.to_ascii_lowercase();
    INGREDIENTS
        .iter()
        .find(|ing| ing.aliases.iter().any(|a| t.contains(a)))
}

/// Volume units → ml. `fl oz` handled after whitespace-normalizing.
fn vol_ml(unit: &str) -> Option<f64> {
    let u = unit.replace(' ', "");
    match u.as_str() {
        "cup" | "cups" => Some(ML_PER_CUP),
        "tbsp" | "tbsps" | "tablespoon" | "tablespoons" => Some(14.786_764_8),
        "tsp" | "tsps" | "teaspoon" | "teaspoons" => Some(4.928_921_59),
        "ml" | "milliliter" | "milliliters" => Some(1.0),
        "l" | "liter" | "liters" => Some(1000.0),
        "floz" | "fl oz" => Some(29.573_529_6),
        _ => None,
    }
}

/// Mass units → g.
fn mass_g(unit: &str) -> Option<f64> {
    match unit {
        "g" | "gram" | "grams" => Some(1.0),
        "kg" | "kilogram" | "kilograms" => Some(1000.0),
        "oz" | "ounce" | "ounces" => Some(28.349_523_125),
        "lb" | "lbs" | "pound" | "pounds" => Some(453.592_37),
        _ => None,
    }
}

/// grams → value in `unit` (for display).
fn mass_to(g: f64, unit: &str) -> f64 {
    let base = mass_g(unit).unwrap_or(1.0);
    g / base
}

/// ml → value in `unit` (for display).
fn vol_to(ml: f64, unit: &str) -> f64 {
    let base = vol_ml(unit).unwrap_or(1.0);
    ml / base
}

fn unit_label(unit: &str) -> &'static str {
    match unit {
        "cup" => "cup",
        "cups" => "cups",
        "tbsp" | "tbsps" | "tablespoon" | "tablespoons" => "tbsp",
        "tsp" | "tsps" | "teaspoon" | "teaspoons" => "tsp",
        "ml" | "milliliter" | "milliliters" => "ml",
        "l" | "liter" | "liters" => "l",
        "floz" | "fl oz" => "fl oz",
        "g" | "gram" | "grams" => "g",
        "kg" | "kilogram" | "kilograms" => "kg",
        "oz" | "ounce" | "ounces" => "oz",
        "lb" | "lbs" | "pound" | "pounds" => "lb",
        _ => "units",
    }
}

static RE_DENSITY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)^\s*([+-]?\d+(?:\.\d+)?(?:/\d+)?)\s*",
        r"(cup|cups|tbsp|tbsps|tablespoon|tablespoons|tsp|tsps|teaspoon|teaspoons|",
        r"ml|milliliter|milliliters|l|liter|liters|fl\s?oz|floz|",
        r"g|gram|grams|kg|kilogram|kilograms|oz|ounce|ounces|lb|lbs|pound|pounds|",
        r"stick|sticks)\s+(?:of\s+)?(.+?)\s*$",
    ))
    .unwrap()
});

/// Split `"flour in cups"` → (`"flour"`, `Some("cups")`).
fn split_target(s: &str) -> (String, Option<String>) {
    static RE_TARGET: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^(.*?)\s+(?:in|as|to)\s+([a-zµ\s]+?)\s*$").unwrap());
    if let Some(c) = RE_TARGET.captures(s) {
        let t = c.get(2).map(|m| m.as_str().trim().to_ascii_lowercase());
        let left = c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        (left, t.filter(|u| !u.is_empty()))
    } else {
        (s.to_string(), None)
    }
}

/// Choose a pleasant default output unit: mass → g (or kg when big),
/// volume → cups.
fn default_mass_unit(g: f64) -> &'static str {
    if g >= 1000.0 { "kg" } else { "g" }
}

/// `100g flour in cups`, `2 cups sugar in g`, `1 stick butter`.
pub(crate) fn try_cooking(q: &str) -> Option<SearchResult> {
    let c = RE_DENSITY.captures(q)?;
    let qty = parse_qty_number(c.get(1)?.as_str())?;
    if qty <= 0.0 {
        return None;
    }
    let unit_raw = c.get(2)?.as_str().to_ascii_lowercase();
    let rest = c.get(3)?.as_str();
    let (ing_str, target) = split_target(rest);
    if ing_str.trim().is_empty() {
        return None;
    }

    // Butter sticks: `1 stick butter` → 113.4 g.
    if unit_raw == "stick" || unit_raw == "sticks" {
        let ing = find_ingredient(&ing_str)?;
        if !ing.aliases.contains(&"butter") {
            return None;
        }
        let mass = qty * G_PER_BUTTER_STICK;
        let shown = q.trim().to_string();
        let (out, out_label) = match target.as_deref() {
            Some(t) if mass_g(t).is_some() => (mass_to(mass, t), unit_label(t)),
            _ => (mass, "g"),
        };
        let title = format!("{} {}", format_number(out), out_label);
        let subtitle = format!("{} stick{} butter = {} · {} g/stick", format_number(qty), if qty == 1.0 { "" } else { "s" }, title, G_PER_BUTTER_STICK);
        return Some(card_result(
            title.clone(),
            subtitle,
            format!("{} = {}", shown, title),
            shown,
            "butter",
            title,
            "grams",
        ));
    }

    let ing = find_ingredient(&ing_str)?;
    let density = ing.g_per_cup / ML_PER_CUP; // g/ml

    // Normalize both ways so any density pair is computable.
    let v_ml = vol_ml(&unit_raw).map(|ml| qty * ml);
    let mass = mass_g(&unit_raw).map(|g| qty * g);
    let (v_ml, g_mass) = match (v_ml, mass) {
        (Some(v), None) => (v, v * density),
        (None, Some(m)) => (m / density, m),
        _ => return None,
    };

    // Pick output unit: explicit target, else the natural opposite.
    let (out, out_label) = if let Some(t) = target {
        if mass_g(&t).is_some() {
            (mass_to(g_mass, &t), unit_label(&t))
        } else if vol_ml(&t).is_some() {
            (vol_to(v_ml, &t), unit_label(&t))
        } else {
            return None;
        }
    } else if vol_ml(&unit_raw).is_some() {
        let u = default_mass_unit(g_mass);
        (mass_to(g_mass, u), u)
    } else {
        (vol_to(v_ml, "cups"), "cups")
    };

    let shown = q.trim().to_string();
    let title = format!("{} {}", format_number(out), out_label);
    let from = format!("{} {} {}", format_number(qty), unit_label(&unit_raw), ing.name);
    let subtitle = format!("{from} → {title} · {:.3} g/ml", density);
    Some(card_result(
        title.clone(),
        subtitle,
        format!("{shown} = {title} ({} {}/cup)", ing.name, format_number(ing.g_per_cup)),
        shown,
        "cooking",
        title,
        out_label,
    ))
}

static RE_REST: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)^\s*(\d+(?:\.\d+)?(?:/\d+)?)\s*",
        r"(cup|cups|tbsp|tbsps|tablespoon|tablespoons|tsp|tsps|teaspoon|teaspoons|",
        r"ml|milliliter|milliliters|l|liter|liters|",
        r"g|gram|grams|kg|kilogram|kilograms|oz|ounce|ounces|lb|lbs|pound|pounds)",
        r"\s+(?:of\s+)?(.+?)\s*$",
    ))
    .unwrap()
});

/// Parse a scaling prefix, returning (factor, remaining query).
fn scale_prefix(s: &str) -> Option<(f64, &str)> {
    for (word, factor) in [
        ("double ", 2.0),
        ("triple ", 3.0),
        ("quadruple ", 4.0),
        ("quintuple ", 5.0),
        ("halve ", 0.5),
    ] {
        if let Some(rest) = s.strip_prefix(word) {
            return Some((factor, rest));
        }
    }
    if let Some(rest) = s.strip_prefix("scale ") {
        static RE_SCALE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"(?i)^(\d+(?:\.\d+)?)x?\s+(.+)$").unwrap());
        if let Some(c) = RE_SCALE.captures(rest) {
            let f: f64 = c.get(1)?.as_str().parse().ok()?;
            if f > 0.0 {
                return Some((f, c.get(2)?.as_str()));
            }
        }
        return None;
    }
    static RE_SERVINGS: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^(\d+)\s+servings?\s+(?:to|for)\s+(\d+)\s+(.+)$").unwrap()
    });
    if let Some(c) = RE_SERVINGS.captures(s) {
        let from: f64 = c.get(1)?.as_str().parse().ok()?;
        let to: f64 = c.get(2)?.as_str().parse().ok()?;
        if from > 0.0 {
            return Some((to / from, c.get(3)?.as_str()));
        }
    }
    None
}

/// `double 2 cups flour`, `scale 1.5x 200g rice`, `4 servings to 8 2 cups rice`.
pub(crate) fn try_recipe_scale(q: &str) -> Option<SearchResult> {
    let lower = q.trim().to_ascii_lowercase();
    let (factor, rest) = scale_prefix(&lower)?;
    let c = RE_REST.captures(rest)?;
    let qty = parse_qty_number(c.get(1)?.as_str())?;
    if qty <= 0.0 {
        return None;
    }
    let unit_raw = c.get(2)?.as_str().to_ascii_lowercase();
    let ing = find_ingredient(c.get(3)?.as_str())?;

    let scaled = qty * factor;
    let shown = q.trim().to_string();
    let title = format!("{} {} {}", format_number(scaled), unit_label(&unit_raw), ing.name);

    // Bonus mass note when density is known and the unit is volume.
    let mass_note = vol_ml(&unit_raw).map(|ml| {
        let mass = scaled * ml * (ing.g_per_cup / ML_PER_CUP);
        format!(" ≈ {} {}", format_number(mass), default_mass_unit(mass))
    });
    let subtitle = match mass_note {
        Some(n) => format!("{shown} · {}×{n}", format_number(factor)),
        None => format!("{shown} · {}×", format_number(factor)),
    };
    Some(card_result(
        title.clone(),
        subtitle,
        format!("{} = {}", shown, title),
        shown,
        "scale",
        title,
        "result",
    ))
}

/// `fan 200c to conventional` / `conventional 220 c to fan`. Fan ovens run
/// ~20 °C lower, so fan→conventional adds 20 (and the reverse subtracts).
pub(crate) fn try_oven(q: &str) -> Option<SearchResult> {
    static RE_FAN: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(fan|convection|convect)\s+([+-]?\d+(?:\.\d+)?)\s*([cf])\s+(?:to|in)\s+(conventional|normal|regular|static)\s*$",
        )
        .unwrap()
    });
    static RE_CONV: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^(conventional|normal|regular|static)\s+([+-]?\d+(?:\.\d+)?)\s*([cf])\s+(?:to|in)\s+(fan|convection|convect)\s*$",
        )
        .unwrap()
    });

    let (v, unit, fan_to_conv, target) = {
        let c = RE_FAN.captures(q).or_else(|| RE_CONV.captures(q))?;
        let fan_to_conv = RE_FAN.is_match(q);
        let v: f64 = c.get(2)?.as_str().parse().ok()?;
        let unit = c.get(3)?.as_str().to_ascii_lowercase();
        let target = c.get(4)?.as_str();
        (v, unit, fan_to_conv, target)
    };

    let v_c = match unit.as_str() {
        "c" => v,
        "f" => (v - 32.0) * 5.0 / 9.0,
        _ => return None,
    };
    let out_c = if fan_to_conv { v_c + FAN_OFFSET_C } else { v_c - FAN_OFFSET_C };
    let out = match unit.as_str() {
        "c" => out_c,
        _ => out_c * 9.0 / 5.0 + 32.0,
    };

    let shown = q.trim().to_string();
    let title = format!("{} {}", format_number(out), unit);
    let verb = if fan_to_conv { "fan → conventional" } else { "conventional → fan" };
    let subtitle = format!("{verb} · {}°{}", format_number(FAN_OFFSET_C), if unit == "f" { "F" } else { "C" });
    let badge = match target {
        "fan" | "convection" | "convect" => "fan",
        _ => "conventional",
    };
    Some(card_result(
        title.clone(),
        subtitle,
        format!("{shown} = {title}"),
        shown,
        "oven",
        title,
        badge,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_volume_to_mass() {
        let r = try_cooking("2 cups sugar in g").expect("sugar");
        assert_eq!(r.title, "400 g");
        assert_eq!(r.conversion.as_ref().unwrap().left_badge, "cooking");
        assert_eq!(r.conversion.as_ref().unwrap().right_badge, "g");
        let r = try_cooking("1/2 cup butter in grams").expect("fraction");
        assert_eq!(r.title, "113.5 g");
    }

    #[test]
    fn density_mass_to_volume() {
        let r = try_cooking("100g flour in cups").expect("flour");
        assert_eq!(r.title, "0.8 cups");
        let r = try_cooking("200 grams rice in cups").expect("rice");
        assert_eq!(r.title, "1.0811 cups");
    }

    #[test]
    fn bare_quantity_shows_opposite() {
        let r = try_cooking("2 cups milk").expect("milk bare");
        assert_eq!(r.title, "488 g");
        let r = try_cooking("500g flour").expect("flour bare");
        assert_eq!(r.title, "4 cups");
    }

    #[test]
    fn density_fraction_and_spoons() {
        let r = try_cooking("1/3 cup sugar in g").expect("third cup");
        assert_eq!(r.title, "66.6667 g");
        let r = try_cooking("1/2 cup sugar in g").expect("half cup");
        assert_eq!(r.title, "100 g");
        let r = try_cooking("2 tbsp flour in g").expect("tbsp");
        assert_eq!(r.title, "15.625 g");
    }

    #[test]
    fn butter_sticks() {
        let r = try_cooking("1 stick butter").expect("stick");
        assert_eq!(r.title, "113.4 g");
        let r = try_cooking("2 sticks butter in oz").expect("sticks oz");
        assert_eq!(r.title, "8.0001 oz");
    }

    #[test]
    fn non_cooking_falls_through() {
        assert!(try_cooking("2 cups to ml").is_none());
        assert!(try_cooking("1/2 cup to ml").is_none());
        assert!(try_cooking("100g to cups").is_none());
        assert!(try_cooking("5 km to miles").is_none());
        assert!(try_cooking("2 cups unicorn in g").is_none());
    }

    #[test]
    fn recipe_scaling() {
        let r = try_recipe_scale("double 2 cups flour").expect("double");
        assert_eq!(r.title, "4 cups flour");
        assert!(r.title.contains("flour"));
        let r = try_recipe_scale("triple 200g rice").expect("triple");
        assert_eq!(r.title, "600 g rice");
        let r = try_recipe_scale("scale 1.5x 200g rice").expect("scale");
        assert_eq!(r.title, "300 g rice");
        let r = try_recipe_scale("halve 1/2 cup sugar").expect("halve");
        assert_eq!(r.title, "0.25 cup sugar");
        let r = try_recipe_scale("4 servings to 8 2 cups rice").expect("servings");
        assert_eq!(r.title, "4 cups rice");
        assert!(try_recipe_scale("2 cups flour").is_none());
        assert!(try_recipe_scale("double 3 unicorn juice").is_none());
    }

    #[test]
    fn oven_fan_offset() {
        let r = try_oven("fan 200c to conventional").expect("fan");
        assert_eq!(r.title, "220 c");
        let r = try_oven("fan 180 c to normal").expect("fan spaced");
        assert_eq!(r.title, "200 c");
        let r = try_oven("conventional 220 c to fan").expect("reverse");
        assert_eq!(r.title, "200 c");
        let r = try_oven("fan 400f to conventional").expect("fahrenheit");
        assert_eq!(r.title, "436 f");
        assert!(try_oven("fan 200c").is_none());
        assert!(try_oven("200c to f").is_none());
    }
}
