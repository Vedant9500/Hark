#!/usr/bin/env bash
# Optional runtime nudge (prefer permanent rules.lua change).
set -euo pipefail
# Hyprland keyword form:
hyprctl keyword layerrule "blur, namespace:hark" || true
hyprctl keyword layerrule "ignorealpha 0.80, namespace:hark" || true
hyprctl keyword layerrule "xray off, namespace:hark" || true
echo "Applied runtime layerrules for namespace:hark (restart Hark / reload Hyprland if needed)"
