#!/usr/bin/env bash
# Install Blink from a release directory (or next to this script).
# Safe to re-run. Does not require root.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"
AUTOSTART_DIR="$DATA_HOME/autostart"

BIN_SRC=""
for candidate in \
  "$ROOT/blink" \
  "$ROOT/bin/blink" \
  "$ROOT/usr/bin/blink"
do
  if [[ -x "$candidate" ]]; then
    BIN_SRC="$candidate"
    break
  fi
done

if [[ -z "$BIN_SRC" ]]; then
  echo "error: blink binary not found next to install script" >&2
  echo "expected: $ROOT/blink" >&2
  exit 1
fi

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"

echo "Installing blink → $BIN_DIR/blink"
install -Dm755 "$BIN_SRC" "$BIN_DIR/blink"

# Desktop entry
if [[ -f "$ROOT/blink.desktop" ]]; then
  sed "s|^Exec=.*|Exec=$BIN_DIR/blink|" "$ROOT/blink.desktop" \
    > "$APP_DIR/blink.desktop"
elif [[ -f "$ROOT/share/applications/blink.desktop" ]]; then
  sed "s|^Exec=.*|Exec=$BIN_DIR/blink|" "$ROOT/share/applications/blink.desktop" \
    > "$APP_DIR/blink.desktop"
else
  cat > "$APP_DIR/blink.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Blink
Comment=Raycast-style launcher for Linux
Exec=$BIN_DIR/blink
Icon=blink
Terminal=false
Categories=Utility;
EOF
fi
chmod 644 "$APP_DIR/blink.desktop"

# Icon (optional)
for icon in \
  "$ROOT/blink.svg" \
  "$ROOT/share/icons/hicolor/scalable/apps/blink.svg" \
  "$ROOT/assets/blink.svg"
do
  if [[ -f "$icon" ]]; then
    install -Dm644 "$icon" "$ICON_DIR/blink.svg"
    break
  fi
done

# Optional autostart daemon (off by default — enable with --autostart)
if [[ "${1:-}" == "--autostart" ]]; then
  mkdir -p "$AUTOSTART_DIR"
  cat > "$AUTOSTART_DIR/blink.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Blink (daemon)
Comment=Preload Blink launcher
Exec=$BIN_DIR/blink --daemon
Icon=blink
Terminal=false
Categories=Utility;
X-GNOME-Autostart-enabled=true
EOF
  echo "Autostart enabled: $AUTOSTART_DIR/blink.desktop"
fi

# Refresh desktop database if tools exist
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [[ -d "$DATA_HOME/icons/hicolor" ]]; then
  gtk-update-icon-cache -f "$DATA_HOME/icons/hicolor" 2>/dev/null || true
fi

# Ensure ~/.local/bin is on PATH hint
if ! command -v blink >/dev/null 2>&1; then
  echo
  echo "Note: $BIN_DIR is not on your PATH yet."
  echo "Add this to your shell rc:"
  echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo
echo "Installed Blink."
echo "  binary:  $BIN_DIR/blink"
echo "  desktop: $APP_DIR/blink.desktop"
echo
echo "Start daemon:   blink --daemon &"
echo "Toggle window:  blink"
echo "Optional:       re-run with --autostart to launch daemon on login"
echo
echo "Hyprland example:"
echo "  exec-once = blink --daemon"
echo "  bind = ALT, A, exec, blink"
