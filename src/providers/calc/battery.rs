//! AC / battery status from Linux sysfs (`/sys/class/power_supply`).
//!
//! Queries: `battery`, `power`, `ac`, `charging`, `on battery`, etc.
//! Read-only, event-style (only when the user searches) — no background poll.

use super::util::result_calc;
use crate::providers::SearchResult;
use std::fs;
use std::path::Path;

const POWER_SUPPLY: &str = "/sys/class/power_supply";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

#[derive(Debug, Clone)]
struct BatteryInfo {
    name: String,
    status: String,
    capacity: Option<u8>,
    /// Instantaneous power draw/charge in µW (sysfs `power_now`), if present.
    power_now_uw: Option<u64>,
    energy_now_uwh: Option<u64>,
    energy_full_uwh: Option<u64>,
    technology: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Clone)]
struct PowerSnapshot {
    source: PowerSource,
    ac_online: Option<bool>,
    batteries: Vec<BatteryInfo>,
}

pub(crate) fn try_battery(q: &str) -> Option<SearchResult> {
    if !is_battery_query(q) {
        return None;
    }
    let snap = read_power_snapshot();
    Some(format_result(&snap))
}

fn is_battery_query(q: &str) -> bool {
    let lower = q.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "battery"
            | "batteries"
            | "bat"
            | "power"
            | "power source"
            | "powersource"
            | "power status"
            | "ac power"
            | "on ac"
            | "on battery"
            | "charging"
            | "charger"
            | "plugged"
            | "plugged in"
            | "unplugged"
            | "battery status"
            | "battery level"
            | "battery percent"
            | "battery percentage"
            | "power supply"
    )
}

fn read_power_snapshot() -> PowerSnapshot {
    let mut ac_online: Option<bool> = None;
    let mut batteries = Vec::new();

    let Ok(entries) = fs::read_dir(POWER_SUPPLY) else {
        return PowerSnapshot {
            source: PowerSource::Unknown,
            ac_online: None,
            batteries,
        };
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let supply_type = read_trimmed(&path.join("type")).unwrap_or_default();
        match supply_type.as_str() {
            "Mains" => {
                // Prefer any online AC adapter; keep last if multiple.
                let online = read_trimmed(&path.join("online"))
                    .map(|s| s == "1")
                    .unwrap_or(false);
                ac_online = Some(ac_online.unwrap_or(false) || online);
            }
            "Battery" => {
                // Skip virtual / absent packs
                if read_trimmed(&path.join("present")).as_deref() == Some("0") {
                    continue;
                }
                if let Some(info) = read_battery(&path) {
                    batteries.push(info);
                }
            }
            // USB-C / dock PD sources can act as AC when online.
            "USB" => {
                let online = read_trimmed(&path.join("online"))
                    .map(|s| s == "1")
                    .unwrap_or(false);
                if online {
                    ac_online = Some(true);
                } else if ac_online.is_none() {
                    // Don't force false — real Mains may still set it.
                }
            }
            _ => {}
        }
    }

    // Sort batteries by name for stable UI (BAT0 before BAT1).
    batteries.sort_by(|a, b| a.name.cmp(&b.name));

    // Prefer explicit AC `online`; otherwise infer from battery status.
    // "Not charging" usually means plugged in but charge limited / full — treat as AC.
    let source = match ac_online {
        Some(true) => PowerSource::Ac,
        Some(false) => PowerSource::Battery,
        None => {
            if batteries.is_empty() {
                PowerSource::Unknown
            } else {
                let any_discharging = batteries
                    .iter()
                    .any(|b| b.status.eq_ignore_ascii_case("discharging"));
                let any_on_ac = batteries.iter().any(|b| {
                    let s = b.status.to_ascii_lowercase();
                    matches!(s.as_str(), "charging" | "full" | "charged" | "not charging")
                });
                if any_discharging && !any_on_ac {
                    PowerSource::Battery
                } else if any_on_ac {
                    PowerSource::Ac
                } else if any_discharging {
                    PowerSource::Battery
                } else {
                    PowerSource::Unknown
                }
            }
        }
    };

    PowerSnapshot {
        source,
        ac_online,
        batteries,
    }
}

fn read_battery(path: &Path) -> Option<BatteryInfo> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("BAT")
        .to_string();
    let status = read_trimmed(&path.join("status")).unwrap_or_else(|| "Unknown".into());
    let capacity = read_trimmed(&path.join("capacity")).and_then(|s| s.parse().ok());
    let power_now_uw = read_u64(&path.join("power_now"));
    // Prefer energy (µWh); fall back to charge (µAh) * voltage for rough energy.
    let energy_now_uwh = read_u64(&path.join("energy_now")).or_else(|| {
        let charge = read_u64(&path.join("charge_now"))?;
        let voltage = read_u64(&path.join("voltage_now"))?;
        Some(charge.saturating_mul(voltage) / 1_000_000)
    });
    let energy_full_uwh = read_u64(&path.join("energy_full")).or_else(|| {
        let charge = read_u64(&path.join("charge_full"))?;
        let voltage = read_u64(&path.join("voltage_now"))?;
        Some(charge.saturating_mul(voltage) / 1_000_000)
    });
    let technology = read_trimmed(&path.join("technology"));
    let model = read_trimmed(&path.join("model_name"));

    Some(BatteryInfo {
        name,
        status,
        capacity,
        power_now_uw,
        energy_now_uwh,
        energy_full_uwh,
        technology,
        model,
    })
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

fn format_result(snap: &PowerSnapshot) -> SearchResult {
    let (title, icon) = match snap.source {
        PowerSource::Ac => {
            let pct = primary_capacity(snap);
            let title = match pct {
                Some(p) => format!("On AC power · battery {p}%"),
                None => "On AC power".into(),
            };
            (title, battery_icon_for(pct, true))
        }
        PowerSource::Battery => {
            let pct = primary_capacity(snap);
            let title = match pct {
                Some(p) => format!("On battery · {p}%"),
                None => "On battery".into(),
            };
            (title, battery_icon_for(pct, false))
        }
        PowerSource::Unknown => {
            if snap.batteries.is_empty() && snap.ac_online.is_none() {
                ("Power status unavailable".into(), "battery-missing")
            } else {
                let pct = primary_capacity(snap);
                let title = match pct {
                    Some(p) => format!("Battery {p}%"),
                    None => "Power status unknown".into(),
                };
                (title, battery_icon_for(pct, false))
            }
        }
    };

    let subtitle = build_subtitle(snap);
    let copy = build_copy_text(snap, &title);

    // Prefer battery-themed icons when the theme has them; util helper uses calc icon.
    let mut r = result_calc(title, subtitle, copy);
    r.icon = Some(icon.into());
    r.id = format!("power:{}", r.id);
    // Slightly above generic calc so it wins pure keyword matches.
    r.score = 15_000;
    r
}

fn primary_capacity(snap: &PowerSnapshot) -> Option<u8> {
    snap.batteries.first().and_then(|b| b.capacity)
}

fn battery_icon_for(capacity: Option<u8>, charging: bool) -> &'static str {
    let level = capacity.unwrap_or(50);
    let base = if level <= 10 {
        "battery-caution"
    } else if level <= 20 {
        "battery-low"
    } else if level <= 40 {
        "battery-good"
    } else {
        "battery-full"
    };
    if charging {
        // Common FreeDesktop names; theme may only have base.
        match base {
            "battery-caution" => "battery-caution-charging",
            "battery-low" => "battery-low-charging",
            "battery-good" => "battery-good-charging",
            _ => "battery-full-charging",
        }
    } else {
        base
    }
}

fn build_subtitle(snap: &PowerSnapshot) -> String {
    if snap.batteries.is_empty() {
        return match snap.ac_online {
            Some(true) => "AC adapter connected · no battery reported".into(),
            Some(false) => "AC adapter offline · no battery reported".into(),
            None => "No power_supply nodes found (desktop / VM?)".into(),
        };
    }

    let mut parts = Vec::new();
    for b in &snap.batteries {
        let mut s = format!("{}: {}", b.name, human_status(&b.status));
        if let Some(p) = b.capacity {
            s.push_str(&format!(" · {p}%"));
        }
        if let Some(rate) = format_power_rate(b) {
            s.push_str(" · ");
            s.push_str(&rate);
        }
        if let Some(eta) = estimate_time(b, snap.source) {
            s.push_str(" · ");
            s.push_str(&eta);
        }
        parts.push(s);
    }

    if snap.batteries.len() == 1 {
        let b = &snap.batteries[0];
        let mut extra = Vec::new();
        if let Some(ref tech) = b.technology {
            if !tech.is_empty() && tech != "Unknown" {
                extra.push(tech.clone());
            }
        }
        if let Some(ref model) = b.model {
            if !model.is_empty() {
                extra.push(model.clone());
            }
        }
        if let (Some(now), Some(full)) = (b.energy_now_uwh, b.energy_full_uwh) {
            if full > 0 {
                extra.push(format!(
                    "{:.1}/{:.1} Wh",
                    now as f64 / 1_000_000.0,
                    full as f64 / 1_000_000.0
                ));
            }
        }
        if !extra.is_empty() {
            parts.push(extra.join(" · "));
        }
    }

    parts.join(" · ")
}

fn human_status(status: &str) -> String {
    // Sysfs uses Title Case already; normalize a bit.
    let s = status.trim();
    if s.is_empty() {
        "Unknown".into()
    } else {
        s.to_string()
    }
}

fn format_power_rate(b: &BatteryInfo) -> Option<String> {
    let uw = b.power_now_uw.filter(|&p| p > 0)?;
    let w = uw as f64 / 1_000_000.0;
    if w < 0.05 {
        return None;
    }
    Some(format!("{w:.1} W"))
}

fn estimate_time(b: &BatteryInfo, source: PowerSource) -> Option<String> {
    let power = b.power_now_uw.filter(|&p| p > 0)? as f64;
    let energy_now = b.energy_now_uwh? as f64;
    let energy_full = b.energy_full_uwh.unwrap_or(0) as f64;
    if power < 1.0 {
        return None;
    }

    let status = b.status.to_ascii_lowercase();
    let secs = if status == "discharging"
        || matches!(source, PowerSource::Battery) && status != "charging"
    {
        // time to empty
        if energy_now <= 0.0 {
            return None;
        }
        // energy µWh / power µW → hours; convert to seconds
        (energy_now / power) * 3600.0
    } else if status == "charging" {
        let remain = energy_full - energy_now;
        if remain <= 0.0 || energy_full <= 0.0 {
            return None;
        }
        (remain / power) * 3600.0
    } else {
        return None;
    };

    if !secs.is_finite() || secs <= 0.0 || secs > 48.0 * 3600.0 {
        return None;
    }
    Some(format_duration_secs(secs as u64))
}

fn format_duration_secs(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("~{h}h {m:02}m left")
    } else {
        format!("~{m}m left")
    }
}

fn build_copy_text(snap: &PowerSnapshot, title: &str) -> String {
    let mut lines = vec![title.to_string()];
    match snap.ac_online {
        Some(true) => lines.push("AC: connected".into()),
        Some(false) => lines.push("AC: disconnected".into()),
        None => {}
    }
    for b in &snap.batteries {
        let mut line = format!("{}: {}", b.name, b.status);
        if let Some(p) = b.capacity {
            line.push_str(&format!(" ({p}%)"));
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Keywords that must not be treated as plain app/file text in the calc provider.
pub(crate) fn is_battery_keyword(q: &str) -> bool {
    is_battery_query(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_match() {
        assert!(is_battery_query("battery"));
        assert!(is_battery_query("  Power  "));
        assert!(is_battery_query("on ac"));
        assert!(is_battery_query("ac power"));
        assert!(is_battery_query("charging"));
        assert!(!is_battery_query("ac"));
        assert!(!is_battery_query("battery saver"));
        assert!(!is_battery_query("firefox"));
    }

    #[test]
    fn duration_format() {
        assert_eq!(format_duration_secs(90), "~1m left");
        assert_eq!(format_duration_secs(3661), "~1h 01m left");
    }
}
