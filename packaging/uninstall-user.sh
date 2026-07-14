#!/usr/bin/env bash
# Remove a user-local Blink install (no root).
set -euo pipefail

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"
AUTOSTART_DIR="$DATA_HOME/autostart"

pkill -x blink 2>/dev/null || true

rm -f "$BIN_DIR/blink"
rm -f "$APP_DIR/blink.desktop"
rm -f "$ICON_DIR/blink.svg"
rm -f "$AUTOSTART_DIR/blink.desktop"

echo "Blink uninstalled from user directories."
