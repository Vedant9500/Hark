#!/usr/bin/env bash
# Dev install from a source checkout (builds then installs user-local).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"

echo "Building blink (release)…"
cd "$ROOT"

FEATURES=()
if pacman -Q gtk4-layer-shell &>/dev/null \
  || pkg-config --exists gtk4-layer-shell-0 2>/dev/null \
  || dpkg -s libgtk4-layer-shell0 &>/dev/null 2>&1; then
  FEATURES=(--features layer-shell)
  echo "gtk4-layer-shell found — enabling overlay mode"
else
  echo "Note: install gtk4-layer-shell for true Hyprland overlay"
fi

cargo build --release "${FEATURES[@]}"

install -Dm755 "$ROOT/target/release/blink" "$BIN_DIR/blink"
echo "Installed: $BIN_DIR/blink"

# Desktop entry + icon from packaging/
if [[ -f "$ROOT/packaging/blink.desktop" ]]; then
  sed "s|^Exec=.*|Exec=$BIN_DIR/blink|" "$ROOT/packaging/blink.desktop" \
    > "$APP_DIR/blink.desktop"
else
  cat > "$APP_DIR/blink.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Blink
Comment=Raycast-style launcher
Exec=$BIN_DIR/blink
Icon=blink
Terminal=false
Categories=Utility;
EOF
fi

if [[ -f "$ROOT/assets/blink.svg" ]]; then
  install -Dm644 "$ROOT/assets/blink.svg" "$ICON_DIR/blink.svg"
fi

echo
echo "Blink runs as a resident daemon (started at login)."
echo "Hotkey Alt+A only toggles the window — no cold start."
echo
echo "Start now:  pkill -x blink; blink --daemon &"
echo
echo "Shareable package for friends:"
echo "  ./scripts/package-release.sh   # writes dist/*.tar.gz + dist/install.sh"
echo "Done."
