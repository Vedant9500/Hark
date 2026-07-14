#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
mkdir -p "$BIN_DIR"

echo "Building blink (release)…"
cd "$ROOT"

FEATURES=""
if pacman -Q gtk4-layer-shell &>/dev/null || pkg-config --exists gtk4-layer-shell-0 2>/dev/null; then
  FEATURES="--features layer-shell"
  echo "gtk4-layer-shell found — enabling overlay mode"
else
  echo "Note: install gtk4-layer-shell for true Hyprland overlay (sudo pacman -S gtk4-layer-shell)"
fi

cargo build --release $FEATURES

install -Dm755 "$ROOT/target/release/blink" "$BIN_DIR/blink"
echo "Installed: $BIN_DIR/blink"

# Desktop entry
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$APP_DIR"
cat > "$APP_DIR/blink.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Blink
Comment=Raycast-style launcher
Exec=$BIN_DIR/blink
Icon=system-search
Terminal=false
Categories=Utility;
EOF

echo
echo "Blink runs as a resident daemon (started at login)."
echo "Hotkey Alt+A only toggles the window — no cold start."
echo
echo "Start now:  pkill -x blink; blink --daemon &"
echo "Done."
