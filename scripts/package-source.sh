#!/usr/bin/env bash
# Pack a clean, shareable source tree (no GitHub / no git clone needed).
#
# Creates:
#   dist/blink-<ver>-source.tar.gz   ~hundreds of KB (not GB)
#
# Excludes: target/, dist/, .git/, personal scratch, build artifacts.
#
# Usage:
#   ./scripts/package-source.sh
#   ./scripts/package-source.sh /path/to/dropbox-folder
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(
  sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1
)"
PKG_NAME="blink-${VERSION}-source"
DIST="$ROOT/dist"
OUT_DIR="${1:-$DIST}"
STAGE="$(mktemp -d)"
cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT

mkdir -p "$OUT_DIR" "$STAGE/$PKG_NAME"

echo "==> Packaging clean source: ${PKG_NAME}"
echo "    (excludes target/, dist/, .git/, personal files)"

# Prefer git archive when available (respects .gitattributes / tracked files),
# then fall back to rsync/tar of the working tree with explicit excludes.
if git rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  && git rev-parse HEAD >/dev/null 2>&1; then
  # Include tracked + untracked-but-wanted source (git archive only tracks committed).
  # So we always use a curated copy for a complete friend-ready tree.
  :
fi

# Curated copy — complete source regardless of git commit state
# (tar is used instead of rsync so this works on minimal systems)
tar -C "$ROOT" \
  --exclude='./.git' \
  --exclude='./target' \
  --exclude='./dist' \
  --exclude='./.cache' \
  --exclude='./todo.txt' \
  --exclude='./.env' \
  --exclude='./.env.*' \
  --exclude='./config.local.toml' \
  --exclude='./blink.local.toml' \
  --exclude='./packaging/aur/pkg' \
  --exclude='./packaging/aur/src' \
  --exclude='./coverage' \
  --exclude='./.idea' \
  --exclude='./.vscode' \
  --exclude='./perf.data' \
  --exclude='./perf.data.old' \
  --exclude='./flamegraph.svg' \
  --exclude='*.rs.bk' \
  --exclude='*.local.md' \
  --exclude='*.swp' \
  --exclude='*.swo' \
  --exclude='*~' \
  --exclude='.DS_Store' \
  --exclude='Thumbs.db' \
  -cf - . | tar -C "$STAGE/$PKG_NAME" -xf -

# Friend-facing build instructions (no GitHub assumed)
cat > "$STAGE/$PKG_NAME/BUILD_FROM_SOURCE.txt" <<EOF
Blink ${VERSION} — complete source package
==========================================

This archive is the full source code. No git / GitHub required.

1) Install build dependencies
-----------------------------
Arch / Endeavour / CachyOS / Manjaro:
  sudo pacman -S --needed rust gtk4 gtk4-layer-shell pkgconf base-devel

Debian / Ubuntu (names may vary slightly by version):
  sudo apt update
  sudo apt install -y build-essential pkg-config libgtk-4-dev
  # optional overlay for Hyprland:
  #   sudo apt install -y libgtk4-layer-shell-dev   # if available

Fedora:
  sudo dnf install -y rust cargo gtk4-devel pkgconf-pkg-config
  # optional: gtk4-layer-shell-devel

2) Build & install (user-local, no root)
----------------------------------------
  cd ${PKG_NAME}
  ./scripts/install.sh

  # or manually:
  cargo build --release --features layer-shell
  # binary: target/release/blink

3) Run
------
  # preload (recommended)
  blink --daemon &

  # toggle UI (bind this to a hotkey, e.g. Alt+A)
  blink

Hyprland example (hyprland.conf):
  exec-once = blink --daemon
  bind = ALT, A, exec, blink

4) Optional: make a binary package for someone else
---------------------------------------------------
  ./scripts/package-release.sh
  # share dist/blink-*-linux.tar.gz (they do NOT need Rust)

Uninstall (user install):
  rm -f ~/.local/bin/blink
  rm -f ~/.local/share/applications/blink.desktop
  rm -f ~/.local/share/icons/hicolor/scalable/apps/blink.svg
EOF

# Make scripts executable inside the archive
chmod +x \
  "$STAGE/$PKG_NAME/scripts/"*.sh \
  "$STAGE/$PKG_NAME/packaging/install-user.sh" \
  "$STAGE/$PKG_NAME/packaging/uninstall-user.sh" \
  2>/dev/null || true

TARBALL="$OUT_DIR/${PKG_NAME}.tar.gz"
echo "==> Creating $TARBALL"
tar -C "$STAGE" -czf "$TARBALL" "$PKG_NAME"

# Also write a .zip for friends on tools that prefer zip
ZIPBALL="$OUT_DIR/${PKG_NAME}.zip"
if command -v zip >/dev/null 2>&1; then
  echo "==> Creating $ZIPBALL"
  (
    cd "$STAGE"
    zip -qr "$ZIPBALL" "$PKG_NAME"
  )
else
  echo "==> zip not installed — skipping .zip (optional: pacman -S zip)"
fi

# Size report
echo
echo "Done — share either file with your friend:"
ls -lh "$TARBALL" ${ZIPBALL:+"$ZIPBALL"} 2>/dev/null | sed 's/^/  /'
echo
echo "They run:"
echo "  tar xzf ${PKG_NAME}.tar.gz"
echo "  cd ${PKG_NAME}"
echo "  # read BUILD_FROM_SOURCE.txt"
echo "  ./scripts/install.sh"
echo
echo "Tip: send via USB, LocalSend, KDE Connect, Telegram, email, etc."
echo "     Do NOT send the whole project folder (target/ alone is multi-GB)."
