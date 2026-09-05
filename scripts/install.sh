#!/usr/bin/env bash
# Dev install from a source checkout:
#   1) cargo build --release
#   2) install to ~/.local/bin
#   3) restart resident daemon (unless --no-restart)
#
# Usage:
#   ./scripts/install.sh
#   hark update                     # same thing, from the installed binary
#   ./scripts/install.sh --no-restart
#   ./scripts/install.sh --restart-only   # skip build (binary already installed)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"
BIN="$BIN_DIR/hark"

DO_BUILD=1
DO_RESTART=1
for arg in "$@"; do
  case "$arg" in
    --no-restart) DO_RESTART=0 ;;
    --restart-only) DO_BUILD=0; DO_RESTART=1 ;;
    -h|--help)
      sed -n "2,12p" "$0"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: $0 [--no-restart] [--restart-only]" >&2
      exit 1
      ;;
  esac
done

restart_daemon() {
  if [[ ! -x "$BIN" ]]; then
    echo "error: $BIN not found or not executable" >&2
    return 1
  fi

  echo "Restarting hark daemon…"
  # Prefer graceful IPC if the binary supports it; always ensure a clean process.
  if pgrep -x hark >/dev/null 2>&1; then
    pkill -x hark 2>/dev/null || true
    # Wait briefly so the Unix socket is released.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      if ! pgrep -x hark >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
    if pgrep -x hark >/dev/null 2>&1; then
      pkill -9 -x hark 2>/dev/null || true
      sleep 0.1
    fi
  fi

  # Start detached so this script can exit without killing the daemon.
  nohup "$BIN" --daemon >/dev/null 2>&1 &
  disown 2>/dev/null || true
  sleep 0.15
  if pgrep -x hark >/dev/null 2>&1; then
    echo "Daemon running (pid $(pgrep -x hark | tr "\n" " "))."
  else
    echo "warning: daemon did not stay up — try: $BIN --daemon" >&2
    return 1
  fi
}

if [[ "$DO_BUILD" -eq 1 ]]; then
  mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"

  echo "Building hark (release)…"
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

  # If a daemon is holding the old binary, replace after kill so install never hits ETXTBSY.
  if pgrep -x hark >/dev/null 2>&1; then
    echo "Stopping old daemon so the binary can be replaced…"
    pkill -x hark 2>/dev/null || true
    sleep 0.2
  fi

  install -Dm755 "$ROOT/target/release/hark" "$BIN"
  echo "Installed: $BIN"

  # Desktop entry + icon from packaging/
  # (same sed-escaping as packaging/install-user.sh: `&`/`\`/`|` in the
  # path would otherwise inject into the replacement).
  ESC_BIN="${BIN//\\/\\\\}"
  ESC_BIN="${ESC_BIN//|/\\|}"
  ESC_BIN="${ESC_BIN//&/\\&}"
  if [[ -f "$ROOT/packaging/hark.desktop" ]]; then
    sed "s|^Exec=.*|Exec=$ESC_BIN|" "$ROOT/packaging/hark.desktop" \
      > "$APP_DIR/hark.desktop"
  else
    cat > "$APP_DIR/hark.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Hark
Comment=Raycast-style launcher
Exec=$BIN
Icon=hark
Terminal=false
Categories=Utility;
EOF
  fi

  if [[ -f "$ROOT/assets/hark.svg" ]]; then
    install -Dm644 "$ROOT/assets/hark.svg" "$ICON_DIR/hark.svg"
  fi
fi

if [[ "$DO_RESTART" -eq 1 ]]; then
  restart_daemon
else
  echo
  echo "Skipped daemon restart (--no-restart)."
  echo "Start later:  pkill -x hark; $BIN --daemon &"
fi

echo
echo "Hark is a resident daemon — hotkey toggles the window only."
echo "Shareable package: ./scripts/package-release.sh"
echo "Done."
