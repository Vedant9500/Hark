# Hyprland Hark Corners — Blur Halo Troubleshooting

Recurred twice (2026-08-20). Symptom: semi-transparent square window around rounded card, 2-4px triangles at corners, or large 720×480 rectangular dark overlay behind shell (Image1). Previous fix `ee891c9` (405 16:9), regressed after `720×405` fixed-surface ghost experiment; now 720×480 Vicinae/Raycast 1.5-1.6.

## Root Cause

Hyprland blurs rectangular layer surface, not rounded CSS. `ignore_alpha` decides which pixels trigger blur.

* `src/theme/css.rs:49,55` `shell_shadow` — **outer** `box-shadow: 0 16px 48px` (and `0 20px 60px`) was clipped to rectangular layer buffer (`SHELL_INSET 0` `src/ui/mod.rs:47` → buffer == shell). Clipped everywhere except 4 concave corners → paints semi-transparent black square up to buffer edge (faint halo). Fix `css.rs:40-44` comment: **inset-only** `inset 0 1px..., inset 0 0 0 1px` follows radius, no square. Verified `git diff css.rs:40-55`.
* `src/theme/css.rs:54` shell `rgba(surface_container, base)` `base 0.40-1.0` → `0.85` default, `border-radius 14` `src/ui/mod.rs:128` + `overflow Hidden` creates AA `0.1-0.6` and `0` outside radius.
* `~/.config/hypr/hyprland/rules.lua:178` `hl.layer_rule` `ignore_alpha [a]` — wiki https://wiki.hypr.land/Configuring/Basics/Window-Rules/#layer-rules : blur ignores `≤a` (default `0`). Valid per `/usr/share/hypr/stubs/hl.meta.lua:555` `HL.LayerRuleSpec`: `animation, blur, ignore_alpha, xray, no_anim...` — `shadow` invalid → `unknown field 'shadow'` blocked reload, kept `0.82`.
* Low `ignore_alpha 0.05` blurs AA `0.1` → square rect visible (worse Image1 `0.6→0.05`). High `0.8` ignores AA → only `>0.8` interior `0.85` blurred, corners `0` transparent. `xray false` blurs windows+wallpaper, `true` wallpaper only.
* Fixed-surface `src/ui/mod.rs:2850` `set_size_request(720,480)` always left `370px` gap below `110` compact shell; `window != shell` exposed backing at corners. Hug `window+shell 720×480 / 720×110` `src/ui/mod.rs:2050` keeps `window==shell`, no gap.

## Fix (current)

`packaging/hyprland/layer-rules.lua.snippet:8` canonical; `~/.config/hypr/hyprland/rules.lua:178` must match:

```lua
hl.layer_rule({ match={namespace="hark"}, animation="popin 80%", blur=true, ignore_alpha=0.8, xray=false })
-- window_rule no_shadow=true at rules.lua:103; CSS inset-only at css.rs:49,55 (no outer)
```

* `ignore_alpha 0.8` (≈ under `0.85` shell, over AA `0.6`) corners `0` transparent, interior blurred. `0.38` still rang, `0.60` faint, `0.05` blurred square worse, `0.82` also ok but near shell edge. `0.8` current.
* CSS inset-only: `css.rs:49` `inset 0 1px...` / `css.rs:55` same — outer `0 16px/20px 60px` removed (was square at `SHEL_INSET 0`).
* Code hug: `setup_window_chrome:2850` `set_default_size(outer_w,-1)` + `apply_body_chrome:2050` both `window`+`shell` to `480`/`110`; `shell:122` `vexpand true Fill`. Removes `370px` gap.
* Ghost vs corners: fixed-surface (`720×480` always) kills `layersIn slide` ghost `animations.lua:16` but exposes gap; hug + `no_anim` preferred if ghost returns.

## Diagnose Live (no file edit)

Hark must be visible (`hyprctl layers` shows `hark 720×480`):

```bash
hyprctl reload 2>&1 | head   # check "unknown field" error
hyprctl keyword layerrule "ignore_alpha 0.6,match:namespace hark"  # try 0.38,0.6,0.75,0.82
hyprctl keyword layerrule "xray off,match:namespace hark"        # vs on
hyprctl keyword layerrule "blur off,match:namespace hark"        # halo gone? → blur culprit
hyprctl layers -j | jq '.[].levels["2"][] | select(.namespace=="hark")'
```

Check CSS still transparent: `window.hark-window:93` `background-color:transparent` else Adwaita draws square.

## Prevention

* Keep `packaging/hyprland/layer-rules.lua.snippet` and `~/.config/hypr/hyprland/rules.lua` in sync; never use `shadow` in `layer_rule`.
* After editing `src/ui/mod.rs` chrome, `cargo check --features layer-shell` + `hyprctl reload` + restart `~/.local/bin/hark` (manual install) else stale `110`/`480`.
* Test `ignore_alpha` live via `hyprctl keyword` before committing; screenshot corners at `0.05` vs `0.6`.
* If ghost returns with hug, prefer `no_anim` layer rule over fixed-surface.
