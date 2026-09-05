#!/usr/bin/env bash
# Install Hark from a release directory (or next to this script).
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
  "$ROOT/hark" \
  "$ROOT/bin/hark" \
  "$ROOT/usr/bin/hark"
do
  if [[ -x "$candidate" ]]; then
    BIN_SRC="$candidate"
    break
  fi
done

if [[ -z "$BIN_SRC" ]]; then
  echo "error: hark binary not found next to install script" >&2
  echo "expected: $ROOT/hark" >&2
  exit 1
fi

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"

echo "Installing hark → $BIN_DIR/hark"
# tmp+rename: overwriting a running daemon's binary hits ETXTBSY and aborts
# mid-script under `set -e` (audit P3); rename over it succeeds.
TMP_BIN="$BIN_DIR/.hark.new.$$"
install -m755 "$BIN_SRC" "$TMP_BIN"
mv -f "$TMP_BIN" "$BIN_DIR/hark"

# Escape sed-replacement metacharacters in the install path (`&\` and the
# `|` delimiter) so exotic XDG_BIN_HOME values can't inject (audit P3).
ESC_BIN_DIR="${BIN_DIR//\\/\\\\}"
ESC_BIN_DIR="${ESC_BIN_DIR//|/\\|}"
ESC_BIN_DIR="${ESC_BIN_DIR//&/\\&}"
# Quote Exec when the path contains spaces (audit P3).
if [[ "$BIN_DIR" == *" "* ]]; then
  DESKTOP_EXEC="\"$BIN_DIR/hark\""
else
  DESKTOP_EXEC="$BIN_DIR/hark"
fi

# Desktop entry
if [[ -f "$ROOT/hark.desktop" ]]; then
  sed "s|^Exec=.*|Exec=${ESC_BIN_DIR}/hark|" "$ROOT/hark.desktop" \
    > "$APP_DIR/hark.desktop"
elif [[ -f "$ROOT/share/applications/hark.desktop" ]]; then
  sed "s|^Exec=.*|Exec=${ESC_BIN_DIR}/hark|" "$ROOT/share/applications/hark.desktop" \
    > "$APP_DIR/hark.desktop"
else
  cat > "$APP_DIR/hark.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Hark
Comment=Raycast-style launcher for Linux
Exec=$DESKTOP_EXEC
Icon=hark
Terminal=false
Categories=Utility;
EOF
fi
chmod 644 "$APP_DIR/hark.desktop"

# Icon (optional)
for icon in \
  "$ROOT/hark.svg" \
  "$ROOT/share/icons/hicolor/scalable/apps/hark.svg" \
  "$ROOT/assets/hark.svg"
do
  if [[ -f "$icon" ]]; then
    install -Dm644 "$icon" "$ICON_DIR/hark.svg"
    break
  fi
done

# Optional autostart daemon (off by default — enable with --autostart)
if [[ "${1:-}" == "--autostart" ]]; then
  mkdir -p "$AUTOSTART_DIR"
  cat > "$AUTOSTART_DIR/hark.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Hark (daemon)
Comment=Preload Hark launcher
Exec=$DESKTOP_EXEC --daemon
Icon=hark
Terminal=false
Categories=Utility;
X-GNOME-Autostart-enabled=true
EOF
  echo "Autostart enabled: $AUTOSTART_DIR/hark.desktop"
fi

# Refresh desktop database if tools exist
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [[ -d "$DATA_HOME/icons/hicolor" ]]; then
  gtk-update-icon-cache -f "$DATA_HOME/icons/hicolor" 2>/dev/null || true
fi

# Ensure the install dir is on PATH hint
if ! command -v hark >/dev/null 2>&1; then
  echo
  echo "Note: $BIN_DIR is not on your PATH yet."
  echo "Add this to your shell rc:"
  echo "  export PATH=\"$BIN_DIR:\$PATH\""
fi

echo
echo "Installed Hark."
echo "  binary:  $BIN_DIR/hark"
echo "  desktop: $APP_DIR/hark.desktop"
echo
echo "Start daemon:   hark --daemon &"
echo "Toggle window:  hark"
echo "Optional:       re-run with --autostart to launch daemon on login"
echo
echo "Hyprland example:"
echo "  exec-once = hark --daemon"
echo "  bind = ALT, A, exec, hark"
