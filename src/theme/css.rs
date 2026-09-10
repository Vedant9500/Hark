use super::{sanitize_hex, Theme};

/// Expand a sanitized hex colour to its 6-digit RGB form.
///
/// `sanitize_hex` accepts `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`; the alpha
/// nibble/byte (when present) is dropped because callers take alpha as a
/// separate parameter. Anything unexpected falls back to the dark-shell
/// default components.
fn rgb_bytes(hex: &str) -> (u8, u8, u8) {
    let h = sanitize_hex(hex);
    let h = h.trim_start_matches('#');
    // Sanitized output is ASCII hex, so byte indexing is always at a char
    // boundary. Previous version collected `Vec<char>` + `format!` per call
    // (~15 allocs per `render`); this parses in place with zero allocation.
    let b = h.as_bytes();
    match b.len() {
        3 | 4 => {
            // Shorthand: duplicate each nibble (`#fff` → `ffffff`).
            // `d * 17` == `0xdd` for a hex nibble d (0xF → 255).
            (
                hex_nibble(b[0]) * 17,
                hex_nibble(b[1]) * 17,
                hex_nibble(b[2]) * 17,
            )
        }
        6 | 8 => {
            let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(26);
            let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(27);
            let bl = u8::from_str_radix(&h[4..6], 16).unwrap_or(38);
            (r, g, bl)
        }
        _ => (26, 27, 38),
    }
}

#[inline]
fn hex_nibble(b: u8) -> u8 {
    (b as char).to_digit(16).unwrap_or(0) as u8
}

pub fn is_light_theme(hex: &str) -> bool {
    // scheme.json values are external input — sanitize before byte slicing.
    let (r, g, b) = rgb_bytes(hex);
    let lum = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
    lum > 128.0
}

fn rgba(hex: &str, alpha: f32) -> String {
    // scheme.json values are external input — sanitize before byte slicing.
    let (r, g, b) = rgb_bytes(hex);
    format!("rgba({r}, {g}, {b}, {alpha})")
}

pub fn render(theme: &Theme, ui: &crate::config::UiThemeConfig) -> String {
    // `clamp` passes NaN through (all comparisons false), which would emit
    // `NaNpx` / `rgba(..., NaN)` — invalid CSS that GTK drops. JSON cannot
    // express NaN, but guard anyway so a corrupt in-memory value degrades to
    // defaults instead of an unstyled panel.
    let base = if ui.opacity.is_finite() {
        ui.opacity.clamp(0.40, 1.0)
    } else {
        0.85
    };
    // `ui.accent` is external input (config file / settings): sanitize on the
    // render path too, not just in `UiThemeConfig::sanitize`. The raw value
    // is interpolated directly into `caret-color` / badge rules, so an
    // unsanitized override would be a CSS-injection sink if any caller ever
    // skips the store sanitize step. Store path already restricts to
    // `#rrggbb|None`; this keeps the `pub` API safe by construction.
    let primary_raw = ui
        .accent
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(theme.primary.as_str());
    let primary = sanitize_hex(primary_raw);
    let scale = if ui.font_scale.is_finite() {
        ui.font_scale.clamp(0.85, 1.30)
    } else {
        1.0
    };
    let radius = ui.radius.clamp(8, 24);
    // Optical concentricity: R_inner = max(4, R_outer - padding_h)
    let row_radius = radius.saturating_sub(6).clamp(4, 18);
    let icon_size = ui.icon_size.clamp(18, 36);

    let is_light = is_light_theme(&theme.surface_container);
    // Inset-only shadow. An OUTER shadow here is the square-corner artifact:
    // the layer surface buffer == the shell's bounding rect (SHELL_INSET 0),
    // so an outer shadow is clipped everywhere except the four concave corner
    // regions, where it paints semi-transparent black up to the buffer edge —
    // a sharp square block over the wallpaper (docs/hyprland-layer-corners.md).
    let (border, border_soft, shell_shadow) = if is_light {
        (
            rgba(&theme.outline_variant, 0.85),
            rgba(&theme.outline_variant, 0.70),
            "box-shadow: inset 0 1px 0 0 rgba(255, 255, 255, 0.60), inset 0 0 0 1px rgba(0, 0, 0, 0.08);",
        )
    } else {
        (
            rgba(&theme.outline_variant, 0.75),
            rgba(&theme.outline_variant, 0.62),
            "box-shadow: inset 0 1px 0 0 rgba(255, 255, 255, 0.12), inset 0 0 0 1px rgba(255, 255, 255, 0.05);",
        )
    };
    // Selected-row bevel: single-scanline inset top light, zero blur.
    // Blurred outer shadows force an offscreen repaint per row per scroll
    // frame; a 1px inset is one cached fill. Light themes get a stronger
    // white glint, dark themes a faint one (black casts are invisible).
    let row_bevel = if is_light {
        "box-shadow: inset 0 1px 0 0 rgba(255, 255, 255, 0.50);"
    } else {
        "box-shadow: inset 0 1px 0 0 rgba(255, 255, 255, 0.09);"
    };

    let shell_bg = rgba(&theme.surface_container, base);
    // Transparent twin of shell_bg for the settings scroll-edge fades.
    let shell_bg_clear = rgba(&theme.surface_container, 0.0);
    // Half-strength head of the fade gradient (user-tuned: full shell tone
    // read too heavy over rows). Kept whisper-thin so the scroll scrim never
    // reads as a shadow bar — 0.28 head over 16px.
    let shell_bg_half = rgba(&theme.surface_container, base * 0.28);
    // Popovers float over results/previews without Hyprland blur — need higher opacity.
    let popover_bg = rgba(&theme.surface_container, (base + 0.32).min(0.94));
    let popover_bg_solid = rgba(&theme.surface_container_high, (base + 0.42).min(0.97));
    let search_bg = rgba(&theme.surface_container_high, (base + 0.05).min(1.0));
    let hover_bg = rgba(&theme.on_surface, 0.06);
    let selected_bg = rgba(&primary, 0.18);
    // Selected row: clearly above hover (0.06 vs 0.14) so the active item
    // reads at a glance. Same on_surface wash language — no bright solid
    // block over the semi-transparent shell. Edge comes from a 1px border
    // + inset top glint in the rule below, not from blur.
    let row_selected_bg = rgba(&theme.on_surface, 0.14);
    // Icon tile: faint flat fill that unifies mixed app/folder glyphs into
    // one rhythm. Static background-color + 1px border, no blur, no shadow.
    let icon_tile_bg = rgba(&theme.on_surface, 0.07);
    let hint = &theme.on_surface_variant;
    let empty = &theme.on_surface_variant;
    let subtitle = &theme.on_surface_variant;
    let on_surface = &theme.on_surface;
    let conv_badge_bg = rgba(&theme.on_surface, 0.08);
    // Placeholder / leading icon are blended text, not `on_surface_variant`
    // (that hint color renders 1.89:1 on the dark shell — fails WCAG).
    // on_surface @0.55 renders 3.8:1, @0.62 (icon) 4.4:1. Measure contrast on
    // gamma-space blends: CSS composites alpha in sRGB gamma, not linear
    // light (a linear-light blend would falsely read ≈5.4:1).
    let placeholder = rgba(&theme.on_surface, 0.55);
    let icon_color = rgba(&theme.on_surface, 0.62);

    // Scaled type sizes (base @ scale 1.0).
    let fs = |px: f32| -> String { format!("{:.1}px", px * scale) };
    let search_fs = fs(18.0);
    let title_fs = fs(14.0);
    let subtitle_fs = fs(12.0);
    let badge_fs = fs(11.0);
    let preview_title_fs = fs(13.0);
    let preview_meta_fs = fs(11.0);
    let preview_code_fs = fs(11.0);
    let empty_fs = fs(12.0);

    format!(
        r#"
window.hark-window {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  outline: none;
  padding: 0;
  margin: 0;
}}

/* Frame is flush with the window — no square transparent "padding". */
window.hark-window .hark-frame {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  padding: 0;
  margin: 0;
}}

/* Kill default Adwaita fills that paint square under the rounded card. */
window.hark-window > *,
window.hark-window .hark-frame,
window.hark-window .hark-frame > *,
window.hark-window .hark-shell > *,
window.hark-window .hark-shell stack,
window.hark-window .hark-shell stack > * {{
  background-image: none;
}}

/* Panel shell — single rounded card with dual-layer glass rim highlight.
   Subtle vertical shade (one static gradient, no blur): top catches light,
   bottom grounds the card so the list reads as sitting INSIDE it. */
window.hark-window .hark-shell {{
  background-color: {shell_bg};
  background-image: linear-gradient(to bottom, rgba(255, 255, 255, 0.04), rgba(0, 0, 0, 0.08));
  border: 1px solid {border};
  border-radius: {radius}px;
  {shell_shadow}
  padding: 0;
  margin: 0;
  /* Compact list width; Rust grows the window only when preview opens. */
  min-width: 720px;
}}

/* Stack / pages must stay transparent so only the shell paints the card. */
window.hark-window .hark-shell > stack,
window.hark-window .hark-shell > stack > * {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  padding: 0;
  margin: 0;
  min-height: 0;
}}

/* --- Header / search (Raycast: flush top, no boxed field) --- */
window.hark-window .hark-header {{
  padding: 14px 16px 6px 16px;
  background-color: transparent;
}}

window.hark-window .hark-search {{
  background-color: transparent;
  background-image: none;
  border: none;
  border-radius: 0;
  padding: 2px 4px;
  font-size: {search_fs};
  font-weight: 500;
  color: {on_surface};
  caret-color: {primary};
  outline: none;
  box-shadow: none;
  min-height: 28px;
}}

window.hark-window .hark-search:focus {{
  border: none;
  outline: none;
  box-shadow: none;
  background-color: transparent;
}}

window.hark-window .hark-search placeholder {{
  color: {placeholder};
  opacity: 1;
}}

/* Leading search icon: legible tint + scales with the search font size so
   accessibility font scales don't leave a tiny 16px glyph next to 23px text. */
window.hark-window .hark-search image {{
  color: {icon_color};
  min-width: {search_fs};
  min-height: {search_fs};
}}

/* Async deep/translate spinner lives in the secondary-icon slot (image.right).
   `.hark-search-busy` is toggled from Rust; the class gate keeps the static
   clear button from spinning when idle. */
@keyframes hark-icon-spin {{
  to {{ transform: rotate(1turn); }}
}}

window.hark-window .hark-search.hark-search-busy image.right,
window.hark-window .hark-search.hark-search-busy image:last-child {{
  animation: hark-icon-spin 1.1s linear infinite;
}}

/* Hero card picker wheel: direction-aware slide (GTK Stack) + subtle pop so the
   value change reads as intentional, not a flicker. Stack handles slide/crossfade;
   the card container gets a quick opacity pop via class toggled from Rust. */
@keyframes hark-card-pop {{
  0% {{ opacity: 0.86; }}
  100% {{ opacity: 1; }}
}}
@keyframes hark-arrow-nudge {{
  0% {{ opacity: 0.7; }}
  50% {{ opacity: 1; }}
  100% {{ opacity: 0.7; }}
}}

/* App-side open/close pop. Compositor layer animation is off (`no_anim`
   layerrule): box interpolation ghosts on surface resize, so entrance/exit
   animate the card INSIDE the surface — the box never changes, nothing can
   ghost (docs/hyprland-layer-corners.md). Classes toggled from Rust. */
@keyframes hark-shell-in {{
  from {{ opacity: 0; transform: scale(0.96); }}
  to {{ opacity: 1; transform: scale(1); }}
}}
@keyframes hark-shell-out {{
  from {{ opacity: 1; }}
  to {{ opacity: 0; }}
}}
window.hark-window .hark-shell.hark-anim-in {{
  animation: hark-shell-in 180ms cubic-bezier(0.22, 1, 0.36, 1);
}}
window.hark-window .hark-shell.hark-anim-out {{
  animation: hark-shell-out 110ms ease-in forwards;
}}

/* Separators between search / body / footer */
window.hark-window .hark-sep {{
  background-color: {border_soft};
  min-height: 1px;
  margin: 0;
  opacity: 0.9;
}}

/* --- Results body --- */
window.hark-window .hark-body {{
  /* Horizontal 0: the viewport + edge fades span the full window width;
     rows keep their own 6px side margin so highlights stay inset by design. */
  padding: 2px 0;
  background-color: transparent;
  /* Vicinae 770×480 / Raycast 750×474 → 720×480 (1.50) → body ~390px (480-90).
     720×405 16:9 too short, 720×540 4:3 too tall. Fits preview 380. */
  min-height: 390px;
  transition: min-height 180ms ease;
}}

/* Compact idle: body is hidden; kill min-height so shell hugs search+footer */
window.hark-window .hark-body.hark-body-collapsed {{
  min-height: 0;
  padding: 0;
}}

/* Fluid height expansion wrapper — must stay transparent so only the shell
   paints the card while the body slides down. */
window.hark-window .hark-body-revealer {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  padding: 0;
  margin: 0;
}}

window.hark-window .hark-list-col {{
  min-width: 0;
  padding: 0;
}}

/* --- Right-side media / detail preview --- */
window.hark-window .hark-preview-sep {{
  background-color: {border_soft};
  min-width: 1px;
  margin: 4px 0;
  opacity: 0.9;
}}

window.hark-window .hark-preview {{
  min-width: 280px;
  padding: 10px 12px 12px 12px;
  background-color: transparent;
}}

window.hark-window .hark-preview-stack {{
  background-color: transparent;
}}

window.hark-window .hark-preview-body {{
  padding: 4px 2px;
}}

window.hark-window .hark-preview-empty {{
  color: {empty};
  font-size: {empty_fs};
  opacity: 0.7;
  line-height: 1.4;
}}

window.hark-window .hark-preview-icon {{
  margin-bottom: 4px;
  opacity: 0.95;
}}

window.hark-window .hark-preview-badge {{
  font-size: 10px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: {primary};
  background-color: {selected_bg};
  border-radius: 999px;
  padding: 2px 8px;
  margin-bottom: 2px;
}}

window.hark-window .hark-preview-title {{
  font-size: {preview_title_fs};
  font-weight: 600;
  color: {on_surface};
}}

window.hark-window .hark-preview-sub {{
  font-size: {preview_meta_fs};
  color: {subtitle};
  opacity: 0.9;
}}

window.hark-window .hark-preview-meta {{
  font-size: {preview_meta_fs};
  color: {hint};
  opacity: 0.85;
  line-height: 1.35;
}}

window.hark-window .hark-preview-meta-block {{
  padding: 4px 2px 0 2px;
}}

window.hark-window .hark-preview-code-scroll {{
  background-color: transparent;
}}

window.hark-window .hark-preview-code {{
  font-family: monospace;
  font-size: {preview_code_fs};
  background-color: transparent;
}}

window.hark-window .hark-preview-code text {{
  color: {on_surface};
}}

window.hark-window .hark-preview-picture {{
  border-radius: 10px;
  background-color: {hover_bg};
  /* 4:3 frame (248×186 inside 280px panel) */
  min-width: 248px;
  min-height: 186px;
}}

window.hark-window .hark-row-icon {{
  background-color: {icon_tile_bg};
  border: 1px solid {border_soft};
  border-radius: 8px;
  padding: 4px;
  margin-right: 4px;
  opacity: 1;
  min-width: {icon_size}px;
  min-height: {icon_size}px;
}}

window.hark-window .hark-scroll {{
  background-color: transparent;
  border: none;
  box-shadow: none;
  padding: 0;
  margin: 0;
}}

window.hark-window .hark-results {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  outline: none;
  margin: 0;
  padding: 0;
}}

window.hark-window .hark-results > row {{
  background-color: transparent;
  background-image: none;
  /* Transparent 1px border reserves the selected edge so focus never
     shifts layout by a pixel when the border tints in. */
  border: 1px solid transparent;
  outline: none;
  box-shadow: none;
  border-radius: {row_radius}px;
  padding: 0;
  /* Horizontal inset keeps the rounded highlight off the preview separator
     and window edges; 6px also lands row text near the search field's 16px. */
  margin: 2px 6px;
  /* let content define height — fixed min-height was clipping glyphs */
  min-height: 52px;
}}

window.hark-window .hark-row-inner {{
  border-radius: {row_radius}px;
  padding: 9px 12px;
  background-color: transparent;
}}

window.hark-window .hark-results > row:hover {{
  background-color: {hover_bg};
}}

window.hark-window .hark-results > row:selected,
window.hark-window .hark-results > row:selected:hover {{
  background-color: {row_selected_bg};
  border: 1px solid {border};
  {row_bevel}
  outline: none;
}}

/* Subtitle sits back (0.70) so the title leads; selection lifts it slightly
   without ever matching title brightness — brightness = closeness. */
window.hark-window .hark-results > row:selected .hark-subtitle {{
  opacity: 0.9;
}}

window.hark-window .hark-title {{
  color: {on_surface};
  font-size: {title_fs};
  font-weight: 600;
  /* avoid glyph tops/bottoms being clipped by tight allocation */
  min-height: 18px;
  padding-top: 1px;
  padding-bottom: 1px;
}}

window.hark-window .hark-subtitle {{
  color: {subtitle};
  font-size: {subtitle_fs};
  opacity: 0.7;
  min-height: 16px;
  padding-top: 1px;
  padding-bottom: 1px;
}}

window.hark-window .hark-badge {{
  background-color: transparent;
  color: {hint};
  border-radius: 0;
  padding: 0 2px;
  font-size: {badge_fs};
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  opacity: 0.65;
}}

window.hark-window .hark-badge.calc,
window.hark-window .hark-badge.file,
window.hark-window .hark-badge.folder {{
  background-color: transparent;
  color: {hint};
}}

/* --- Raycast-style conversion card: hero of the list, so it wears the
   same bordered-card language as a selected row (never the faint hover
   wash). One static gradient + 1px inset glint, zero blur. */
window.hark-window .hark-results > row.hark-conv-row {{
  margin: 2px 6px 8px 6px;
  border-radius: 12px;
  min-height: 0;
  padding: 0;
}}

window.hark-window .hark-results > row.hark-conv-row:selected,
window.hark-window .hark-results > row.hark-conv-row:selected:hover {{
  background-color: transparent;
}}

window.hark-window .hark-conv-card {{
  background-color: {row_selected_bg};
  background-image: linear-gradient(to bottom, rgba(255, 255, 255, 0.05), rgba(0, 0, 0, 0.06));
  border: 1px solid {border};
  {row_bevel}
  border-radius: 12px;
  padding: 10px 14px 14px 14px;
  margin: 0;
  transition: background-color 140ms ease, border-color 140ms ease;
}}

window.hark-window .hark-conv-card.hark-conv-swap {{
  animation: hark-card-pop 200ms ease;
}}
window.hark-window .hark-conv-card.hark-conv-swap .hark-conv-arrow {{
  animation: hark-arrow-nudge 200ms ease;
}}

window.hark-window .hark-results > row.hark-conv-row:selected .hark-conv-card,
window.hark-window .hark-results > row.hark-conv-row:hover .hark-conv-card {{
  background-color: {selected_bg};
  border-color: {border};
}}

window.hark-window .hark-conv-header {{
  color: {hint};
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  opacity: 0.65;
  margin-bottom: 2px;
}}

window.hark-window .hark-conv-panels {{
  min-height: 72px;
  padding: 4px 0;
}}

window.hark-window .hark-conv-panel {{
  padding: 8px 12px;
  min-width: 120px;
}}

window.hark-window .hark-conv-arrow {{
  color: {hint};
  font-size: 22px;
  font-weight: 500;
  opacity: 0.7;
  padding: 0 10px;
  min-width: 36px;
  transition: color 140ms ease, opacity 140ms ease;
}}

window.hark-window .hark-results > row.hark-conv-row:selected .hark-conv-arrow,
window.hark-window .hark-results > row.hark-conv-row:hover .hark-conv-arrow {{
  color: {primary};
  opacity: 1.0;
}}

/* Answer is the hero: right panel dominates, left expression reads as muted
   context. Tabular figures stop digit jitter while typing. */
window.hark-window .hark-conv-title {{
  color: {on_surface};
  font-weight: 600;
  letter-spacing: -0.2px;
  font-feature-settings: "tnum" 1;
}}

window.hark-window .hark-conv-left .hark-conv-title {{
  color: {subtitle};
  font-size: 16px;
  font-weight: 500;
}}

window.hark-window .hark-conv-right .hark-conv-title {{
  font-size: 26px;
  font-weight: 700;
}}

window.hark-window .hark-conv-badge {{
  background-color: {conv_badge_bg};
  color: {hint};
  border-radius: 8px;
  padding: 3px 8px;
  font-size: 11px;
  font-weight: 500;
  opacity: 0.8;
  font-feature-settings: "tnum" 1;
}}

/* --- Footer (Raycast action bar) --- */
window.hark-window .hark-footer {{
  padding: 7px 12px;
  background-color: transparent;
  min-height: 34px;
}}

window.hark-window .hark-footer-primary {{
  padding: 0 2px;
}}

window.hark-window .hark-footer-action {{
  color: {on_surface};
  font-size: 12px;
  font-weight: 500;
  opacity: 0.85;
}}

window.hark-window .hark-footer-actions {{
  padding: 0;
}}

window.hark-window .hark-footer-div {{
  color: {hint};
  font-size: 11px;
  opacity: 0.28;
  padding: 0 10px;
}}

window.hark-window .hark-keycap {{
  background-color: {hover_bg};
  color: {on_surface};
  border: 1px solid {border_soft};
  border-radius: 6px;
  padding: 2px 6px;
  font-size: 10.5px;
  font-weight: 600;
  min-width: 14px;
  opacity: 1;
  letter-spacing: 0.01em;
}}

window.hark-window .hark-action-chip {{
  background-color: transparent;
  border-radius: 6px;
  padding: 2px 4px;
}}

window.hark-window .hark-action-btn {{
  background-color: transparent;
  border: none;
  box-shadow: none;
  padding: 2px 4px;
  border-radius: 6px;
}}

window.hark-window .hark-action-btn:hover {{
  background-color: {hover_bg};
}}

window.hark-window .hark-action-label {{
  color: {on_surface};
  font-size: 12px;
  font-weight: 500;
  opacity: 0.72;
}}

/* --- Action panel (Ctrl+K) / Open With --- */
/* Only paint `contents` — painting the popover + contents creates a double card. */
popover.hark-action-panel {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  padding: 0;
  margin: 0;
  opacity: 1;
}}

popover.hark-action-panel > arrow {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  min-width: 0;
  min-height: 0;
  margin: 0;
  padding: 0;
  opacity: 0;
}}

popover.hark-action-panel > contents,
.hark-action-panel contents {{
  background-color: {popover_bg};
  background-image: none;
  border: 1px solid {border};
  border-radius: 12px;
  box-shadow: none;
  padding: 6px;
  margin: 0;
  opacity: 1;
}}

/* Open With sits over previews — denser single fill */
popover.hark-open-with > contents {{
  background-color: {popover_bg_solid};
  border: 1px solid {border};
}}

.hark-action-panel-inner {{
  min-width: 280px;
  background-color: transparent;
  background-image: none;
}}

popover.hark-action-panel scrolledwindow,
popover.hark-action-panel list,
popover.hark-action-panel viewport,
popover.hark-action-panel overshoot,
popover.hark-action-panel undershoot {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
}}

/* Stronger text contrast in floating menus */
popover.hark-action-panel .hark-action-panel-label {{
  color: {on_surface};
  opacity: 1;
  font-weight: 600;
}}

popover.hark-action-panel .hark-action-panel-shortcut {{
  color: {hint};
  opacity: 0.85;
}}

popover.hark-action-panel .hark-action-panel-header {{
  color: {on_surface};
  opacity: 0.9;
}}

.hark-action-panel-header {{
  color: {hint};
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.04em;
  opacity: 0.75;
  padding: 2px 4px 4px 4px;
}}

.hark-action-panel-list {{
  background-color: transparent;
  border: none;
}}

/* ListBox rows (legacy) + Button rows (current panel). */
.hark-action-panel-list > row,
button.hark-action-panel-row {{
  background-color: transparent;
  background-image: none;
  border-radius: 8px;
  margin: 1px 0;
  padding: 2px 4px;
  border: none;
  outline: none;
  box-shadow: none;
  min-height: 0;
}}

.hark-action-panel-list > row:hover,
button.hark-action-panel-row:hover {{
  background-color: {hover_bg};
}}

.hark-action-panel-list > row:selected,
.hark-action-panel-list > row:selected:hover,
button.hark-action-panel-row.selected,
button.hark-action-panel-row.selected:hover,
button.hark-action-panel-row:focus {{
  background-color: {selected_bg};
  border: 1px solid {border};
}}

.hark-action-panel-label {{
  color: {on_surface};
  font-size: 13px;
  font-weight: 500;
}}

.hark-action-panel-label.destructive {{
  color: #f7768e;
}}

.hark-action-panel-row.destructive:selected .hark-action-panel-label,
button.hark-action-panel-row.destructive.selected .hark-action-panel-label,
button.hark-action-panel-row.destructive:focus .hark-action-panel-label {{
  color: #f7768e;
}}

.hark-action-panel-shortcut {{
  color: {hint};
  font-size: 11px;
  font-weight: 500;
  opacity: 0.65;
}}

window.hark-window .hark-hint {{
  color: {hint};
  font-size: 11px;
  font-weight: 500;
  opacity: 0.8;
  padding: 0;
  margin: 0;
}}

window.hark-window .hark-empty {{
  color: {empty};
  font-size: 13px;
  padding: 32px 24px;
  opacity: 0.9;
  min-height: 160px;
  transition: opacity 140ms ease;
  line-height: 1.5;
}}

window.hark-window .hark-results {{
  transition: opacity 140ms ease;
}}

window.hark-window scrolledwindow {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  padding: 0;
  margin: 0;
}}

window.hark-window scrolledwindow viewport,
window.hark-window scrolledwindow overshoot,
window.hark-window scrolledwindow undershoot,
window.hark-window scrolledwindow junction {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
}}

window.hark-window scrollbar,
window.hark-window scrollbar * {{
  opacity: 0;
  min-width: 0;
  min-height: 0;
  margin: 0;
  padding: 0;
  border: none;
  background: none;
  box-shadow: none;
}}

/* --- Settings panel (Vicinae / Raycast dual pane) --- */
window.hark-window .hark-settings {{
  background-color: transparent;
  min-width: 720px;
  /* Match EXPANDED_WINDOW_HEIGHT — a smaller minimum shrinks the window
     when switching pages (stack is vhomogeneous=false). */
  min-height: 480px;
}}

window.hark-window .hark-settings-split {{
  background-color: transparent;
  min-height: 460px;
}}

window.hark-window .hark-settings-nav-col {{
  background-color: transparent;
  min-width: 200px;
}}

window.hark-window .hark-settings-search {{
  background-color: {hover_bg};
  color: {on_surface};
  border: 1px solid {border_soft};
  border-radius: 10px;
  padding: 6px 10px;
  min-height: 28px;
  font-size: 12.5px;
  caret-color: {primary};
  outline: none;
  box-shadow: none;
}}

window.hark-window .hark-settings-search:focus {{
  border-color: {border};
  background-color: {search_bg};
  outline: none;
  box-shadow: none;
}}

window.hark-window .hark-settings-search placeholder {{
  color: {hint};
  opacity: 0.7;
}}

window.hark-window .hark-settings-nav-scroll {{
  background-color: transparent;
  min-width: 196px;
}}

window.hark-window .hark-settings-nav {{
  background-color: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  padding: 2px 8px 8px 8px;
}}

window.hark-window .hark-settings-nav-row {{
  background-color: transparent;
  background-image: none;
  border: none;
  border-radius: 9px;
  margin: 1px 0;
  padding: 0;
  outline: none;
  box-shadow: none;
}}

window.hark-window .hark-settings-nav-row:hover {{
  background-color: {hover_bg};
}}

window.hark-window .hark-settings-nav-row:selected,
window.hark-window .hark-settings-nav-row:selected:hover {{
  background-color: {selected_bg};
}}

window.hark-window .hark-settings-nav-item {{
  background: transparent;
}}

window.hark-window .hark-settings-nav-icon {{
  color: {hint};
  opacity: 0.9;
}}

window.hark-window .hark-settings-nav-row:selected .hark-settings-nav-icon {{
  color: {primary};
  opacity: 1;
}}

window.hark-window .hark-settings-nav-title {{
  color: {on_surface};
  font-size: 13px;
  font-weight: 500;
}}

window.hark-window .hark-settings-nav-row:selected .hark-settings-nav-title {{
  font-weight: 600;
}}

window.hark-window .hark-settings-nav-footer {{
  background: transparent;
}}

window.hark-window .hark-settings-nav-footer-label {{
  color: {hint};
  font-size: 11px;
  opacity: 0.8;
}}

window.hark-window .hark-settings-done {{
  font-size: 12px;
  font-weight: 600;
  padding: 3px 10px;
  min-height: 0;
}}

window.hark-window .hark-settings-content-stack {{
  background-color: transparent;
  min-width: 480px;
}}

window.hark-window .hark-settings-page {{
  background-color: transparent;
}}

window.hark-window .hark-settings-page-header {{
  background: transparent;
}}

/* Scroll-edge fades: whisper scrims so rows never read as touching the
   viewport rim. Shared by settings + results list. Opacity driven per-view
   from scroll position; 16px / 0.28 head so it cues scroll without reading
   as a shadow bar between search / list / footer. */
window.hark-window .hark-fade-top {{
  background-image: linear-gradient(to bottom, {shell_bg_half}, {shell_bg_clear});
  min-height: 16px;
}}

window.hark-window .hark-fade-bottom {{
  background-image: linear-gradient(to top, {shell_bg_half}, {shell_bg_clear});
  min-height: 16px;
}}

window.hark-window .hark-settings-body {{
  background-color: transparent;
}}

window.hark-window .hark-settings-page-title {{
  color: {on_surface};
  font-size: 15px;
  font-weight: 650;
}}

window.hark-window .hark-settings-page-sub {{
  color: {hint};
  font-size: 12px;
  opacity: 0.88;
}}

window.hark-window .hark-settings-section {{
  color: {primary};
  font-size: 11px;
  font-weight: 650;
  letter-spacing: 0.04em;
  margin-top: 2px;
  margin-bottom: 2px;
  opacity: 0.95;
}}

window.hark-window .hark-settings-card {{
  background-color: {hover_bg};
  border: 1px solid {border_soft};
  border-radius: 12px;
  padding: 0;
}}

window.hark-window .hark-settings-card > separator {{
  background-color: {border_soft};
  min-height: 1px;
  margin: 0;
  opacity: 0.7;
}}

window.hark-window .hark-settings-list {{
  min-height: 40px;
}}

window.hark-window .hark-settings-list-row {{
  padding: 10px 14px;
  min-height: 0;
}}

window.hark-window .hark-settings-list-label {{
  color: {on_surface};
  font-size: 13px;
  font-weight: 500;
}}

window.hark-window .hark-settings-list-sub {{
  color: {hint};
  font-size: 11.5px;
  opacity: 0.88;
}}

window.hark-window .hark-settings-card-footer {{
  padding: 8px 14px 10px 14px;
}}

window.hark-window .hark-settings-stepper-val {{
  color: {on_surface};
  font-size: 13px;
  font-weight: 600;
  min-width: 20px;
}}

window.hark-window .hark-settings-entry {{
  background-color: {search_bg};
  color: {on_surface};
  border: 1px solid {border_soft};
  border-radius: 8px;
  padding: 6px 10px;
  min-height: 28px;
}}

window.hark-window .hark-settings-btn {{
  background-color: {hover_bg};
  color: {on_surface};
  border-radius: 8px;
  padding: 4px 10px;
  border: 1px solid {border_soft};
  font-size: 12.5px;
}}

window.hark-window .hark-settings-btn:hover {{
  background-color: {selected_bg};
  border-color: {border};
}}

window.hark-window .hark-settings-icon-btn {{
  min-width: 28px;
  padding: 2px 8px;
  font-size: 14px;
}}

window.hark-window .hark-settings-primary {{
  background-color: {selected_bg};
  color: {on_surface};
  font-weight: 600;
  padding: 6px 12px;
  border: 1px solid {border};
}}

window.hark-window .hark-settings-link {{
  color: {hint};
  font-size: 11px;
  background: none;
  border: none;
  padding: 0 6px;
}}

window.hark-window .hark-settings-link:hover {{
  color: {primary};
}}

window.hark-window .hark-settings-check,
window.hark-window .hark-settings-radio {{
  color: {on_surface};
  margin: 0;
}}

window.hark-window checkbutton {{
  color: {on_surface};
  margin: 0;
}}

window.hark-window checkbutton label {{
  color: {on_surface};
  font-size: 13px;
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiThemeConfig;

    #[test]
    fn test_is_light_theme() {
        assert!(!is_light_theme("#24283b"));
        assert!(!is_light_theme("#1a1b26"));
        assert!(is_light_theme("#ffffff"));
        assert!(is_light_theme("#f0f0f0"));
    }

    #[test]
    fn test_shorthand_and_rgba_hex_expand() {
        // `#fff` is white, not the dark fallback (audit P3).
        assert!(is_light_theme("#fff"));
        assert_eq!(rgba("#fff", 0.5), "rgba(255, 255, 255, 0.5)");
        assert_eq!(rgba("#ffff", 0.5), "rgba(255, 255, 255, 0.5)");
        assert_eq!(rgba("#000", 1.0), "rgba(0, 0, 0, 1)");
        // 8-digit RGBA drops the alpha byte (alpha is a separate param).
        assert_eq!(rgba("#24283bff", 0.5), "rgba(36, 40, 59, 0.5)");
        assert!(!is_light_theme("#24283bff"));
    }

    #[test]
    fn test_render_css_contains_rim_highlight() {
        let theme = Theme::fallback();
        let ui = UiThemeConfig::default();
        let css = render(&theme, &ui);
        assert!(css.contains("inset 0 1px 0 0"));
        assert!(css.contains("window.hark-window .hark-shell"));
    }

    #[test]
    fn test_selected_row_stronger_than_hover() {
        let theme = Theme::fallback();
        let ui = UiThemeConfig::default();
        let css = render(&theme, &ui);
        let alpha_of = |rule: &str| -> f32 {
            let bg = rule.split("background-color:").nth(1).unwrap();
            // Only the background declaration — the selected rule now also
            // carries a bordered edge + inset glint with their own rgba().
            let decl = bg.split(';').next().unwrap();
            let start = decl.rfind(',').unwrap() + 1;
            let end = decl.find(')').unwrap();
            decl[start..end].trim().parse().unwrap()
        };
        let hover_rule = css
            .split("window.hark-window .hark-results > row:hover {")
            .nth(1)
            .unwrap()
            .split("}\n")
            .next()
            .unwrap();
        let sel_rule = css
            .split("window.hark-window .hark-results > row:selected,")
            .nth(1)
            .unwrap()
            .split("}\n")
            .next()
            .unwrap();
        // Same on_surface wash language as hover, but a clear step stronger so
        // the active row is distinguishable without reading as a bright block.
        assert!(sel_rule.contains("background-color: rgba("), "{sel_rule}");
        assert!(alpha_of(sel_rule) > alpha_of(hover_rule), "{sel_rule}");
    }

    #[test]
    fn test_selected_row_bevel_without_blur() {
        let theme = Theme::fallback();
        let ui = UiThemeConfig::default();
        let css = render(&theme, &ui);
        let sel_rule = css
            .split("window.hark-window .hark-results > row:selected,")
            .nth(1)
            .unwrap()
            .split("}\n")
            .next()
            .unwrap();
        // Edge from 1px border + single-scanline inset glint — zero blur
        // radius, so no offscreen repaint per row on scroll.
        assert!(sel_rule.contains("border: 1px solid"), "{sel_rule}");
        assert!(sel_rule.contains("inset 0 1px 0 0"), "{sel_rule}");
        let hover_rule = css
            .split("window.hark-window .hark-results > row:hover {")
            .nth(1)
            .unwrap()
            .split("}\n")
            .next()
            .unwrap();
        // Hover stays a flat wash: no border, no shadow.
        assert!(!hover_rule.contains("box-shadow"), "{hover_rule}");
        // No blurred outer shadows anywhere on rows (flatten + jank).
        assert!(!css.contains(".hark-results > row {\n  background-color: transparent;\n  background-image: none;\n  border: none;"));
    }

    #[test]
    fn test_row_icon_tile_is_flat() {
        let theme = Theme::fallback();
        let ui = UiThemeConfig::default();
        let css = render(&theme, &ui);
        let icon_rule = css
            .split("window.hark-window .hark-row-icon {")
            .nth(1)
            .unwrap()
            .split("}\n")
            .next()
            .unwrap();
        assert!(icon_rule.contains("background-color: rgba("), "{icon_rule}");
        assert!(icon_rule.contains("border: 1px solid"), "{icon_rule}");
        assert!(!icon_rule.contains("box-shadow"), "{icon_rule}");
    }

    #[test]
    fn test_concentric_row_radius() {
        let theme = Theme::fallback();
        let ui = UiThemeConfig {
            radius: 16,
            ..Default::default()
        };
        let css = render(&theme, &ui);
        assert!(css.contains("border-radius: 10px;"));
    }

    #[test]
    fn test_search_placeholder_and_icon_contrast() {
        let theme = Theme::fallback();
        let ui = UiThemeConfig::default();
        let css = render(&theme, &ui);
        // Placeholder now blends on_surface (≈5:1) instead of the dim hint (1.78:1).
        let placeholder_rule = css
            .split("window.hark-window .hark-search placeholder")
            .nth(1)
            .unwrap()
            .split("}\n")
            .next()
            .unwrap();
        assert!(!placeholder_rule.contains("{hint}"), "{placeholder_rule}");
        // Icons get an explicit color + font-scale-linked size.
        assert!(css.contains("window.hark-window .hark-search image"));
        assert!(css.contains("min-height: 18.0px"));
    }

    #[test]
    fn test_search_busy_spinner_animation() {
        let theme = Theme::fallback();
        let ui = UiThemeConfig::default();
        let css = render(&theme, &ui);
        assert!(css.contains("@keyframes hark-icon-spin"));
        assert!(css.contains(".hark-search-busy image.right"));
    }

    #[test]
    fn test_external_non_ascii_scheme_values_do_not_panic() {
        // scheme.json is external input; the byte slicing in is_light_theme /
        // rgba must never see a non-char-boundary index.
        let mut theme = Theme::fallback();
        theme.surface_container = "éé".into(); // 4 bytes, not ASCII hex
        theme.outline_variant = "日本".into();
        let ui = UiThemeConfig::default();
        // Sanitized fallback (#ffffff) reads as light.
        assert!(is_light_theme(&theme.surface_container));
        assert_eq!(
            rgba(&theme.surface_container, 0.5),
            "rgba(255, 255, 255, 0.5)"
        );
        assert_eq!(
            rgba(&theme.outline_variant, 0.5),
            "rgba(255, 255, 255, 0.5)"
        );
        let _ = render(&theme, &ui); // must not panic
    }

    #[test]
    fn test_malicious_accent_override_is_sanitized() {
        // `ui.accent` interpolates directly into caret-color / badge rules —
        // a raw override would be a CSS-injection sink if any caller skips
        // `UiThemeConfig::sanitize`. Render must sanitize by construction.
        let theme = Theme::fallback();
        let evil = "red; } .pwned { color: black";
        let ui = UiThemeConfig {
            accent: Some(evil.into()),
            ..Default::default()
        };
        let css = render(&theme, &ui);
        assert!(!css.contains(evil), "raw accent leaked into CSS");
        // Malformed accent falls back to sanitized white, never empty.
        assert!(css.contains("caret-color: #ffffff;"), "{css}");
    }

    #[test]
    fn test_non_finite_opacity_and_scale_fall_back() {
        // `f32::clamp` passes NaN through; render must not emit `NaNpx` or
        // `rgba(..., NaN)` (GTK drops the whole declaration → unstyled panel).
        let theme = Theme::fallback();
        let ui = UiThemeConfig {
            opacity: f32::NAN,
            font_scale: f32::INFINITY,
            ..Default::default()
        };
        let css = render(&theme, &ui);
        assert!(!css.contains("NaN"), "{css}");
        // `infinite` (spinner keyframes) legitimately contains "inf" — only
        // reject non-finite *values* leaking into numeric declarations.
        assert!(!css.contains(", inf)"), "{css}");
        assert!(!css.contains("infpx"), "{css}");
        // Finite fallbacks still produce a usable shell rule.
        assert!(css.contains("window.hark-window .hark-shell"));
    }

    #[test]
    fn test_shorthand_nibble_expansion_matches_longhand() {
        // `#abc` == `#aabbcc` == (170, 187, 204); guards the `* 17` rewrite.
        assert_eq!(rgba("#abc", 1.0), rgba("#aabbcc", 1.0));
        assert_eq!(rgba("#abc", 1.0), "rgba(170, 187, 204, 1)");
    }
}
