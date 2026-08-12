#!/usr/bin/env bash
# Remove a user-local Hark install (no root).
set -euo pipefail

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"
AUTOSTART_DIR="$DATA_HOME/autostart"

pkill -x hark 2>/dev/null || true

rm -f "$BIN_DIR/hark"
rm -f "$APP_DIR/hark.desktop"
rm -f "$ICON_DIR/hark.svg"
rm -f "$AUTOSTART_DIR/hark.desktop"

echo "Hark uninstalled from user directories."
