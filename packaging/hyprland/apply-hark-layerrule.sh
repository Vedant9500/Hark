#!/usr/bin/env bash
# Optional runtime nudge (prefer the permanent rules.lua snippet).
#
# Hark now syncs a named `hark-blur-sync` rule to the opacity slider at
# runtime, so this helper is only a fallback for compositors/sessions where
# that sync cannot run. Keep the threshold at the default-shell value (0.8);
# Hark lowers it itself when the panel is made more transparent.
set -euo pipefail
# Hyprland 0.53+ keyword form:
hyprctl keyword layerrule "blur on, match:namespace hark" || true
hyprctl keyword layerrule "ignore_alpha 0.8, match:namespace hark" || true
hyprctl keyword layerrule "xray off, match:namespace hark" || true
echo "Applied fallback layerrules for namespace:hark (restart Hark / reload Hyprland if needed)"
