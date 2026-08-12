use super::Theme;

fn rgba(hex: &str, alpha: f32) -> String {
    let h = hex.trim().trim_start_matches('#');
    if h.len() < 6 {
        return format!("rgba(26, 27, 38, {alpha})");
    }
    let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(26);
    let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(27);
    let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(38);
    format!("rgba({r}, {g}, {b}, {alpha})")
}

pub fn render(theme: &Theme, ui: &crate::config::UiThemeConfig) -> String {
    let base = ui.opacity.clamp(0.40, 1.0);
    let primary = ui
        .accent
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(theme.primary.as_str());
    let scale = ui.font_scale.clamp(0.85, 1.30);
    let radius = ui.radius.clamp(8, 24);
    let row_radius = (radius as f32 * 0.5).round().clamp(4.0, 14.0) as u32;
    let icon_size = ui.icon_size.clamp(18, 36);

    let shell_bg = rgba(&theme.surface_container, base);
    // Popovers float over results/previews without Hyprland blur — need higher opacity.
    let popover_bg = rgba(&theme.surface_container, (base + 0.32).min(0.94));
    let popover_bg_solid = rgba(&theme.surface_container_high, (base + 0.42).min(0.97));
    let search_bg = rgba(&theme.surface_container_high, (base + 0.05).min(1.0));
    let hover_bg = rgba(&theme.on_surface, 0.08);
    let selected_bg = rgba(primary, 0.18);
    let border = rgba(&theme.outline_variant, 0.65);
    let border_soft = rgba(&theme.outline_variant, 0.50);
    let hint = &theme.on_surface_variant;
    let empty = &theme.on_surface_variant;
    let subtitle = &theme.on_surface_variant;
    let on_surface = &theme.on_surface;
    let conv_badge_bg = rgba(&theme.on_surface, 0.08);

    // Scaled type sizes (base @ scale 1.0).
    let fs = |px: f32| -> String { format!("{:.1}px", px * scale) };
    let search_fs = fs(18.0);
    let title_fs = fs(14.0);
    let subtitle_fs = fs(12.0);
    let badge_fs = fs(11.0);
    let preview_title_fs = fs(13.0);
    let preview_meta_fs = fs(11.0);
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

/* Panel shell — single rounded card */
window.hark-window .hark-shell {{
  background-color: {shell_bg};
  background-image: none;
  border: 1px solid {border};
  border-radius: {radius}px;
  /* No outer box-shadow: GTK paints it in the rectangular surface and Hyprland
     layer blur turns that into a square "padding" halo. Depth comes from blur. */
  box-shadow: none;
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
  padding: 14px 16px 12px 16px;
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
  color: {hint};
  opacity: 0.75;
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
  padding: 6px 8px;
  background-color: transparent;
  min-height: 120px;
}}

/* Compact idle: body is hidden; kill min-height so shell hugs search+footer */
window.hark-window .hark-body.hark-body-collapsed {{
  min-height: 0;
  padding: 0;
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

window.hark-window .hark-preview-picture {{
  border-radius: 10px;
  background-color: {hover_bg};
  /* 4:3 frame (248×186 inside 280px panel) */
  min-width: 248px;
  min-height: 186px;
}}

window.hark-window .hark-row-icon {{
  margin-right: 2px;
  opacity: 0.95;
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
  border: none;
  outline: none;
  box-shadow: none;
  border-radius: {row_radius}px;
  padding: 0;
  margin: 1px 0;
  /* let content define height — fixed min-height was clipping glyphs */
  min-height: 48px;
}}

window.hark-window .hark-row-inner {{
  border-radius: {row_radius}px;
  padding: 8px 10px;
  background-color: transparent;
}}

window.hark-window .hark-results > row:hover {{
  background-color: {hover_bg};
}}

window.hark-window .hark-results > row:selected,
window.hark-window .hark-results > row:selected:hover {{
  background-color: {selected_bg};
  border: none;
  outline: none;
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
  opacity: 0.9;
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
  font-weight: 500;
  opacity: 0.85;
}}

window.hark-window .hark-badge.calc,
window.hark-window .hark-badge.file,
window.hark-window .hark-badge.folder {{
  background-color: transparent;
  color: {hint};
}}

/* --- Raycast-style conversion card --- */
window.hark-window .hark-results > row.hark-conv-row {{
  margin: 4px 0 8px 0;
  border-radius: 12px;
  min-height: 0;
  padding: 0;
}}

window.hark-window .hark-results > row.hark-conv-row:selected,
window.hark-window .hark-results > row.hark-conv-row:selected:hover {{
  background-color: transparent;
}}

window.hark-window .hark-conv-card {{
  background-color: {hover_bg};
  border: 1px solid {border_soft};
  border-radius: 12px;
  padding: 10px 14px 14px 14px;
  margin: 0 2px;
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
  opacity: 0.85;
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
}}

window.hark-window .hark-conv-title {{
  color: {on_surface};
  font-size: 22px;
  font-weight: 600;
  letter-spacing: -0.2px;
}}

window.hark-window .hark-conv-badge {{
  background-color: {conv_badge_bg};
  color: {hint};
  border-radius: 8px;
  padding: 3px 8px;
  font-size: 11px;
  font-weight: 500;
  opacity: 0.95;
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
  opacity: 0.90;
}}

window.hark-window .hark-footer-actions {{
  padding: 0;
}}

window.hark-window .hark-footer-div {{
  color: {hint};
  font-size: 11px;
  opacity: 0.28;
  padding: 0 6px;
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
  opacity: 0.88;
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
  opacity: 0.82;
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
  padding: 36px 16px;
  opacity: 0.75;
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
  min-height: 400px;
}}

window.hark-window .hark-settings-split {{
  background-color: transparent;
  min-height: 380px;
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

window.hark-window .hark-settings-page-icon {{
  color: {primary};
  opacity: 0.95;
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
