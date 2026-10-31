//! Which desktop rice Hark mirrors, and where that rice keeps its palette.
//!
//! Caelestia and Impasto use different files and formats. The rice switcher
//! records the active one in `~/.rice/state/current`; that marker is
//! authoritative. Without a marker Hark keeps its long-standing Caelestia
//! behaviour when a scheme exists, and only falls back to Impasto when there
//! is no Caelestia scheme at all.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rice {
    Caelestia,
    Impasto,
}

/// Absolute `XDG_STATE_HOME`, or `~/.local/state`. A relative value would
/// resolve against the daemon's cwd (audit P3), so it is ignored.
pub(super) fn state_home() -> Option<PathBuf> {
    xdg_dir("XDG_STATE_HOME").or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
}

/// Absolute `XDG_CONFIG_HOME`, or `~/.config`.
pub(super) fn config_home() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME").or_else(|| dirs::home_dir().map(|h| h.join(".config")))
}

fn xdg_dir(var: &str) -> Option<PathBuf> {
    let dir = std::env::var(var).ok()?;
    if dir.is_empty() {
        return None;
    }
    let path = PathBuf::from(dir);
    path.is_absolute().then_some(path)
}

pub(super) fn caelestia_scheme() -> Option<PathBuf> {
    state_home().map(|p| p.join("caelestia/scheme.json"))
}

pub(super) fn caelestia_shell() -> Option<PathBuf> {
    config_home().map(|p| p.join("caelestia/shell.json"))
}

/// Impasto's shared state directory (`state.json`, `dynamic-colors.json`,
/// `settings.json`).
pub(super) fn impasto_state_home() -> Option<PathBuf> {
    state_home().map(|p| p.join("quickshell"))
}

pub(super) fn impasto_state() -> Option<PathBuf> {
    impasto_state_home().map(|p| p.join("state.json"))
}

pub(super) fn impasto_dynamic() -> Option<PathBuf> {
    impasto_state_home().map(|p| p.join("dynamic-colors.json"))
}

pub(super) fn impasto_settings() -> Option<PathBuf> {
    impasto_state_home().map(|p| p.join("settings.json"))
}

/// The curated palettes live in QML; this is Impasto's single source of truth
/// for static (non-wallpaper) themes.
pub(super) fn impasto_palettes() -> Option<PathBuf> {
    config_home().map(|p| p.join("quickshell/theme/Palettes.qml"))
}

pub(super) fn rice_marker() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rice/state/current"))
}

pub(super) fn active() -> Rice {
    let marked = rice_marker().and_then(|p| std::fs::read_to_string(p).ok());
    match marked.as_deref().map(str::trim) {
        Some(name) if name.eq_ignore_ascii_case("impasto") => Rice::Impasto,
        Some(name) if name.eq_ignore_ascii_case("caelestia") => Rice::Caelestia,
        _ => {
            let has_caelestia = caelestia_scheme().is_some_and(|p| p.is_file());
            let has_impasto = impasto_state().is_some_and(|p| p.is_file())
                || impasto_dynamic().is_some_and(|p| p.is_file());
            if !has_caelestia && has_impasto {
                Rice::Impasto
            } else {
                Rice::Caelestia
            }
        }
    }
}
