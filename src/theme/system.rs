//! System appearance inputs Hark mirrors at runtime.
//!
//! Two sources matter for the "glass" look:
//! * Caelestia's `shell.json` — whether the desktop wants translucent panels.
//! * Hyprland's blur enable flag — whether a translucent layer will actually
//!   be frosted. Without blur a low-alpha panel is just an unreadable sheet.
//!
//! Blur *strength* (size/passes/noise) is compositor-side and cannot be
//! reproduced in GTK CSS, so it is deliberately not read here.

use serde::Deserialize;
use std::cell::Cell;
use std::process::Command;
use std::time::{Duration, Instant};

use super::source::{self, Rice};

/// Values mirrored from the desktop. Missing/unreadable sources default to
/// "on" so Hark behaves exactly as before outside Caelestia/Hyprland.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SystemStyle {
    pub transparency: bool,
    pub blur: bool,
}

impl Default for SystemStyle {
    fn default() -> Self {
        Self {
            transparency: true,
            blur: true,
        }
    }
}

/// How often the show path re-checks `hyprctl getoption` for a blur toggle.
pub(super) const SYSTEM_TTL: Duration = Duration::from_secs(3);
/// Re-assert the runtime layer rule at least this often. A compositor reload
/// drops runtime Lua rules; re-sending restores the threshold even when the
/// opacity did not change.
const SYNC_TTL: Duration = Duration::from_secs(30);

/// Shipped `ignore_alpha` ceiling: the tuned value for the default 0.85 shell
/// (over the anti-aliased corner edge, under the card fill).
const BLUR_ALPHA_MAX: f32 = 0.80;

pub(super) fn load() -> SystemStyle {
    SystemStyle {
        transparency: transparency(),
        blur: hypr_blur_enabled(),
    }
}

/// The active rice's own transparency opt-out. Caelestia exposes a global
/// switch in `shell.json`; Impasto has no equivalent (its `surfaceStyle` only
/// styles the island), so Hark's opacity slider stays authoritative there.
fn transparency() -> bool {
    match source::active() {
        Rice::Caelestia => caelestia_transparency(),
        Rice::Impasto => true,
    }
}

/// The alpha Hark should actually paint with. Caelestia's transparency switch
/// and Hyprland's blur switch are both opt-outs: a translucent card over an
/// unblurred desktop is the exact illegibility the switch exists to prevent.
pub(super) fn effective_opacity(ui_opacity: f32, style: SystemStyle) -> f32 {
    if style.transparency && style.blur {
        ui_opacity
    } else {
        1.0
    }
}

/// `ignore_alpha` for the current shell alpha.
///
/// Keep the tuned 0.80 ceiling at default opacity, but track the card itself
/// below it: `ignore_alpha` must stay **under** the painted alpha or Hyprland
/// stops blurring the whole surface (audit finding 1.2). 0.02 of headroom
/// keeps the anti-aliased corner rim on the ignored side.
pub(super) fn blur_threshold(opacity: f32) -> f32 {
    let opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        0.85
    };
    (opacity - 0.02).clamp(0.0, BLUR_ALPHA_MAX)
}

#[derive(Debug, Deserialize)]
struct ShellFile {
    #[serde(default)]
    appearance: Option<Appearance>,
}

#[derive(Debug, Deserialize)]
struct Appearance {
    #[serde(default)]
    transparency: Option<Transparency>,
}

#[derive(Debug, Deserialize)]
struct Transparency {
    #[serde(default)]
    enabled: Option<bool>,
}

fn caelestia_transparency() -> bool {
    let Some(path) = source::caelestia_shell() else {
        return true;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return true;
    };
    parse_transparency(&raw)
}

fn parse_transparency(raw: &str) -> bool {
    serde_json::from_str::<ShellFile>(raw)
        .ok()
        .and_then(|f| f.appearance)
        .and_then(|a| a.transparency)
        .and_then(|t| t.enabled)
        .unwrap_or(true)
}

fn hypr_blur_enabled() -> bool {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return true;
    }
    let Ok(out) = Command::new("hyprctl")
        .args(["getoption", "decoration:blur:enabled"])
        .output()
    else {
        return true;
    };
    if !out.status.success() {
        return true;
    }
    parse_hypr_bool(&String::from_utf8_lossy(&out.stdout))
}

fn parse_hypr_bool(raw: &str) -> bool {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix("bool:"))
        .map(|value| value.trim() == "true")
        .unwrap_or(true)
}

thread_local! {
    /// 0 = untried, 1 = Lua eval (Hyprland 0.55+), 2 = new keyword,
    /// 3 = legacy keyword, 4 = unavailable.
    static HYPR_METHOD: Cell<u8> = const { Cell::new(0) };
    /// Last `(threshold percent, sent at)` so stepper clicks do not fork
    /// `hyprctl` when the value is unchanged.
    static LAST_SYNC: Cell<Option<(u8, Instant)>> = const { Cell::new(None) };
}

/// Keep the compositor's Hark layer rule in step with the painted alpha.
///
/// Hyprland 0.55+ exposes a named Lua rule: re-declaring the same name updates
/// it (and it wins over the user's unnamed config rule because rules apply in
/// registration order). Older releases use the `layerrule` keyword.
pub(super) fn sync_blur(opacity: f32) {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    if !opacity.is_finite() {
        return;
    }
    let opacity = opacity.clamp(0.0, 1.0);
    // Fully opaque: Hyprland has nothing to blur through.
    if opacity >= 0.995 {
        return;
    }

    let threshold = blur_threshold(opacity);
    let percent = (threshold * 100.0).round().clamp(0.0, 100.0) as u8;
    let now = Instant::now();
    let due = LAST_SYNC.with(|last| match last.get() {
        None => true,
        Some((last_percent, sent_at)) => last_percent != percent || sent_at.elapsed() >= SYNC_TTL,
    });
    if !due {
        return;
    }

    let lua = lua_rule(threshold);
    let modern = format!("ignore_alpha {threshold:.2}, match:namespace hark");
    let legacy = format!("ignorealpha {threshold:.2}, namespace:hark");
    let method = HYPR_METHOD.with(|m| m.get());
    let ok = match method {
        1 => hypr_eval(&lua),
        2 => hypr_keyword(&modern),
        3 => hypr_keyword(&legacy),
        4 => false,
        _ => {
            if hypr_eval(&lua) {
                HYPR_METHOD.with(|m| m.set(1));
                true
            } else if hypr_keyword(&modern) {
                HYPR_METHOD.with(|m| m.set(2));
                true
            } else if hypr_keyword(&legacy) {
                HYPR_METHOD.with(|m| m.set(3));
                true
            } else {
                HYPR_METHOD.with(|m| m.set(4));
                false
            }
        }
    };
    if ok {
        LAST_SYNC.with(|last| last.set(Some((percent, now))));
    }
}

fn lua_rule(threshold: f32) -> String {
    // Only `ignore_alpha` is owned here. Claiming `blur`/`xray`/`no_anim`
    // would override a user who deliberately disabled blur for this layer.
    format!(
        "hl.layer_rule({{ name = \"hark-blur-sync\", match = {{ namespace = \"hark\" }}, \
         ignore_alpha = {threshold:.2} }})"
    )
}

fn hypr_eval(lua: &str) -> bool {
    let Ok(out) = Command::new("hyprctl").args(["eval", lua]).output() else {
        return false;
    };
    out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "ok"
}

fn hypr_keyword(value: &str) -> bool {
    let Ok(out) = Command::new("hyprctl")
        .args(["keyword", "layerrule", value])
        .output()
    else {
        return false;
    };
    out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "ok"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_switches_force_solid() {
        assert_eq!(effective_opacity(0.50, SystemStyle::default()), 0.50);
        assert_eq!(
            effective_opacity(
                0.50,
                SystemStyle {
                    transparency: false,
                    blur: true
                }
            ),
            1.0
        );
        assert_eq!(
            effective_opacity(
                0.50,
                SystemStyle {
                    transparency: true,
                    blur: false
                }
            ),
            1.0
        );
    }

    #[test]
    fn blur_threshold_stays_under_the_painted_alpha() {
        // Default shell: the tuned 0.80 ceiling.
        assert!((blur_threshold(0.85) - 0.80).abs() < f32::EPSILON);
        // Slider floor: still below the 0.40 fill, or blur dies.
        assert!((blur_threshold(0.40) - 0.38).abs() < f32::EPSILON);
        assert!((blur_threshold(0.70) - 0.68).abs() < f32::EPSILON);
        // Garbage in must not emit NaN into the Lua rule.
        assert!(blur_threshold(f32::NAN).is_finite());
    }

    #[test]
    fn shell_json_transparency_defaults_to_on() {
        assert!(parse_transparency("{}"));
        assert!(parse_transparency("not json"));
        assert!(parse_transparency(r#"{"appearance":{}}"#));
        assert!(parse_transparency(
            r#"{"appearance":{"transparency":{"enabled":true}}}"#
        ));
        assert!(!parse_transparency(
            r#"{"appearance":{"transparency":{"enabled":false}}}"#
        ));
    }

    #[test]
    fn hypr_blur_flag_is_parsed_from_getoption() {
        assert!(parse_hypr_bool("bool: true\nset: true\n"));
        assert!(!parse_hypr_bool("bool: false\nset: true\n"));
        assert!(parse_hypr_bool("unexpected"));
    }

    #[test]
    fn lua_rule_is_named_and_carries_the_threshold() {
        let rule = lua_rule(0.38);
        assert!(rule.contains("name = \"hark-blur-sync\""), "{rule}");
        assert!(rule.contains("ignore_alpha = 0.38"), "{rule}");
        assert!(rule.contains("namespace = \"hark\""), "{rule}");
        // Do not re-enable blur / override unrelated effects.
        assert!(!rule.contains("blur ="), "{rule}");
        assert!(!rule.contains("xray ="), "{rule}");
    }
}
