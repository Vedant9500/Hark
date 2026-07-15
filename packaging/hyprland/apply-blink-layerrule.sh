#!/usr/bin/env bash
# Optional runtime nudge (prefer permanent rules.lua change).
set -euo pipefail
# Hyprland keyword form:
hyprctl keyword layerrule "blur, namespace:blink" || true
hyprctl keyword layerrule "ignorealpha 0.80, namespace:blink" || true
hyprctl keyword layerrule "xray off, namespace:blink" || true
echo "Applied runtime layerrules for namespace:blink (restart Blink / reload Hyprland if needed)"
