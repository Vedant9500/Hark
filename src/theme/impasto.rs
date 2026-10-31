//! Impasto palette reader.
//!
//! Impasto's Quickshell shell keeps its palette in two shapes:
//! * the adaptive (wallpaper-derived) palette in
//!   `~/.local/state/quickshell/dynamic-colors.json`;
//! * curated static palettes in
//!   `~/.config/quickshell/theme/Palettes.qml`, selected by the
//!   `activeTheme` id in `~/.local/state/quickshell/state.json`.
//!
//! Both use the same token names (`background`, `accent`, `text`, …), so one
//! mapping feeds Hark's smaller theme struct either way.

use super::source;
use super::{sanitize_hex, Theme};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Palette tokens Hark understands; anything else in the palette is ignored.
const TOKENS: [&str; 7] = [
    "background",
    "surface",
    "surfaceHover",
    "border",
    "text",
    "textMuted",
    "accent",
];

pub(super) fn load() -> Option<Theme> {
    let active = active_id().unwrap_or_else(|| "adaptive".into());
    let dynamic = read(source::impasto_dynamic());
    let palettes = read(source::impasto_palettes());
    theme_from(&active, dynamic.as_deref(), palettes.as_deref())
}

fn active_id() -> Option<String> {
    let raw = read(source::impasto_state())?;
    #[derive(Deserialize)]
    struct State {
        #[serde(default, rename = "activeTheme")]
        active_theme: Option<String>,
    }
    serde_json::from_str::<State>(&raw).ok()?.active_theme
}

fn read(path: Option<PathBuf>) -> Option<String> {
    std::fs::read_to_string(path?).ok()
}

fn theme_from(active: &str, dynamic: Option<&str>, palettes: Option<&str>) -> Option<Theme> {
    let tokens = if active.is_empty() || active == "adaptive" {
        parse_json_tokens(dynamic?)?
    } else {
        parse_palette_qml(palettes?, active)?
    };
    theme_from_tokens(&tokens)
}

fn parse_json_tokens(raw: &str) -> Option<HashMap<String, String>> {
    let tokens: HashMap<String, String> = serde_json::from_str(raw).ok()?;
    (!tokens.is_empty()).then_some(tokens)
}

/// Pull `key: "#hex"` pairs out of the `colors: { … }` block for one palette.
///
/// The file is a hand-maintained QML list, but the shape is regular: one
/// `id` per entry, followed by a flat `colors` object with no nested braces.
fn parse_palette_qml(raw: &str, id: &str) -> Option<HashMap<String, String>> {
    let marker = format!("id: \"{id}\"");
    let start = raw.find(&marker)?;
    let rest = &raw[start + marker.len()..];
    // Never read into the next palette if this entry has no colours block.
    let entry = match rest.find("id: \"") {
        Some(next) => &rest[..next],
        None => rest,
    };
    let colors_at = entry.find("colors: {")?;
    let after = &entry[colors_at + "colors: {".len()..];
    let block = &after[..after.find('}')?];

    let mut tokens = HashMap::new();
    for pair in block.split(',') {
        let Some((key, value)) = pair.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        if TOKENS.contains(&key) && value.starts_with('#') {
            tokens.insert(key.to_string(), value.to_string());
        }
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn theme_from_tokens(tokens: &HashMap<String, String>) -> Option<Theme> {
    let color = |key: &str| tokens.get(key).map(|v| sanitize_hex(v));
    let surface_container = color("background")?;
    let primary = color("accent")?;
    let surface_container_high = color("surfaceHover")
        .or_else(|| color("surface"))
        .unwrap_or_else(|| surface_container.clone());
    let on_surface = color("text").unwrap_or_else(|| "#ffffff".into());
    let on_surface_variant = color("textMuted").unwrap_or_else(|| on_surface.clone());
    let outline_variant = color("border").unwrap_or_else(|| surface_container_high.clone());

    Some(Theme {
        surface_container,
        surface_container_high,
        on_surface,
        on_surface_variant,
        outline_variant,
        primary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const QML: &str = r##"
    readonly property var list: [
        {
            id: "first",
            name: "First",
            colors: {
                background: "#101010", surface: "#111111", surfaceHover: "#121212",
                border: "#131313", text: "#f0f0f0", textMuted: "#909090",
                accent: "#ff0000", accentHover: "#ff2222", accentText: "#000000",
                red: "#ff3333", green: "#00ff00", yellow: "#ffff00", blue: "#0000ff"
            }
        },
        {
            id: "second",
            name: "Second",
            colors: {
                background: "#1e1e2e", surface: "#181825", surfaceHover: "#313244",
                border: "#45475a", text: "#cdd6f4", textMuted: "#a6adc8",
                accent: "#89b4fa", accentHover: "#b4befe", accentText: "#11111b",
                red: "#f38ba8", green: "#a6e3a1", yellow: "#f9e2af", blue: "#89b4fa"
            }
        }
    ]
"##;

    #[test]
    fn static_palette_is_read_by_id() {
        let tokens = parse_palette_qml(QML, "second").unwrap();
        assert_eq!(tokens["background"], "#1e1e2e");
        assert_eq!(tokens["accent"], "#89b4fa");
        assert_eq!(tokens["textMuted"], "#a6adc8");

        let theme = theme_from("second", Some("{}"), Some(QML)).unwrap();
        assert_eq!(theme.surface_container, "#1e1e2e");
        assert_eq!(theme.surface_container_high, "#313244");
        assert_eq!(theme.on_surface, "#cdd6f4");
        assert_eq!(theme.on_surface_variant, "#a6adc8");
        assert_eq!(theme.outline_variant, "#45475a");
        assert_eq!(theme.primary, "#89b4fa");
    }

    #[test]
    fn unknown_static_id_does_not_read_the_next_entry() {
        assert!(parse_palette_qml(QML, "missing").is_none());
        assert!(theme_from("missing", None, Some(QML)).is_none());
    }

    #[test]
    fn adaptive_palette_comes_from_dynamic_colors_json() {
        let json = r##"{
            "background": "#17181a", "surface": "#252628",
            "surfaceHover": "#35363a", "border": "#45464e",
            "text": "#eef2f7", "textMuted": "#94a1b2",
            "accent": "#9c9328", "accentHover": "#c2b83d",
            "accentText": "#11111b", "red": "#f38ba8"
        }"##;
        let theme = theme_from("adaptive", Some(json), None).unwrap();
        assert_eq!(theme.surface_container, "#17181a");
        assert_eq!(theme.surface_container_high, "#35363a");
        assert_eq!(theme.on_surface, "#eef2f7");
        assert_eq!(theme.on_surface_variant, "#94a1b2");
        assert_eq!(theme.outline_variant, "#45464e");
        assert_eq!(theme.primary, "#9c9328");
    }

    #[test]
    fn missing_optional_tokens_fall_back_to_hark_defaults() {
        let json = r##"{"background":"#101010","accent":"#abcdef"}"##;
        let theme = theme_from("adaptive", Some(json), None).unwrap();
        assert_eq!(theme.surface_container_high, "#101010");
        assert_eq!(theme.on_surface, "#ffffff");
        assert_eq!(theme.outline_variant, "#101010");
    }

    #[test]
    fn malformed_json_or_qml_returns_none() {
        assert!(theme_from("adaptive", Some("not json"), None).is_none());
        assert!(theme_from("adaptive", Some("{}"), None).is_none());
        assert!(theme_from("second", Some("{}"), Some("nonsense")).is_none());
    }
}
