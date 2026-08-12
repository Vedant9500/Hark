#!/usr/bin/env bash
# Build shareable Hark release artifacts under dist/.
#
# Outputs:
#   dist/hark-<ver>-x86_64-linux.tar.gz   portable package + install.sh
#   dist/install.sh                        one-line online installer
#   dist/SHA256SUMS
#   dist/*.deb                             if cargo-deb is installed
#
# Usage:
#   ./scripts/package-release.sh
#   HARK_GITHUB_REPO=you/hark ./scripts/package-release.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(
  sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1
)"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
esac
OS=linux
PKG_NAME="hark-${VERSION}-${ARCH}-${OS}"
DIST="$ROOT/dist"
STAGE="$DIST/stage/$PKG_NAME"
GITHUB_REPO="${HARK_GITHUB_REPO:-}"

if [[ -z "$GITHUB_REPO" ]]; then
  # Try git remote
  if remote_url="$(git remote get-url origin 2>/dev/null || true)"; then
    # git@github.com:user/repo.git or https://github.com/user/repo.git
    GITHUB_REPO="$(
      printf '%s\n' "$remote_url" \
        | sed -E 's#^git@github\.com:##; s#^https?://github\.com/##; s#\.git$##'
    )"
  fi
fi
if [[ -z "$GITHUB_REPO" || "$GITHUB_REPO" == *"github.com"* ]]; then
  GITHUB_REPO="Vedant9500/Hark"
fi

echo "==> Packaging Hark v${VERSION} (${ARCH}-${OS})"
echo "    GitHub repo: ${GITHUB_REPO}"

rm -rf "$DIST/stage"
mkdir -p "$STAGE" "$DIST"

# Build with layer-shell when available (recommended for Hyprland)
FEATURES=()
if pkg-config --exists gtk4-layer-shell-0 2>/dev/null \
  || pacman -Q gtk4-layer-shell &>/dev/null \
  || dpkg -s libgtk4-layer-shell0 &>/dev/null 2>&1 \
  || [[ "${HARK_FORCE_LAYER_SHELL:-}" == "1" ]]; then
  FEATURES=(--features layer-shell)
  echo "==> Enabling layer-shell feature"
else
  echo "==> layer-shell not found at build time — binary will use window mode"
  echo "    Install gtk4-layer-shell for Hyprland overlay, or set HARK_FORCE_LAYER_SHELL=1"
fi

echo "==> cargo build --release --locked ${FEATURES[*]:-}"
# --locked: release artifacts must reproduce exactly from the committed Cargo.lock.
cargo build --release --locked "${FEATURES[@]}"

BIN="$ROOT/target/release/hark"
if [[ ! -x "$BIN" ]]; then
  echo "error: missing $BIN" >&2
  exit 1
fi

# Stage package contents
install -Dm755 "$BIN" "$STAGE/hark"
install -Dm755 "$ROOT/packaging/install-user.sh" "$STAGE/install.sh"
install -Dm755 "$ROOT/packaging/uninstall-user.sh" "$STAGE/uninstall.sh"
install -Dm644 "$ROOT/packaging/hark.desktop" "$STAGE/hark.desktop"
install -Dm644 "$ROOT/assets/hark.svg" "$STAGE/hark.svg"
install -Dm644 "$ROOT/LICENSE" "$STAGE/LICENSE"
install -Dm644 "$ROOT/README.md" "$STAGE/README.md"

# Short package README
cat > "$STAGE/INSTALL.txt" <<EOF
Hark ${VERSION} — portable Linux package
========================================

Quick install (no root):
  ./install.sh

Optional login autostart:
  ./install.sh --autostart

Uninstall:
  ./uninstall.sh

Requirements:
  - Linux ${ARCH}
  - GTK 4 runtime (libgtk-4)
  - Recommended for Hyprland: gtk4-layer-shell

Then:
  hark --daemon &
  # bind a hotkey to: hark
EOF

TARBALL="$DIST/${PKG_NAME}.tar.gz"
echo "==> Creating $TARBALL"
tar -C "$DIST/stage" -czf "$TARBALL" "$PKG_NAME"

# Online installer (downloads latest tarball from GitHub Releases)
ONLINE_INSTALLER="$DIST/install.sh"
cat > "$ONLINE_INSTALLER" <<EOF
#!/usr/bin/env bash
# Hark online installer — downloads the latest release and installs user-local.
# Usage:
#   curl -fsSL https://github.com/${GITHUB_REPO}/releases/latest/download/install.sh | bash
#   curl -fsSL ... | bash -s -- --autostart
set -euo pipefail

REPO="${GITHUB_REPO}"
VERSION="${VERSION}"
ARCH_RAW="\$(uname -m)"
case "\$ARCH_RAW" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *)
    echo "error: unsupported arch: \$ARCH_RAW" >&2
    exit 1
    ;;
esac

ASSET="hark-\${VERSION}-\${ARCH}-linux.tar.gz"
# Prefer exact version asset; fall back to latest redirect layout
BASE="https://github.com/\${REPO}/releases"
URL_VERSIONED="\${BASE}/download/v\${VERSION}/\${ASSET}"
URL_LATEST="\${BASE}/latest/download/\${ASSET}"

TMP="\$(mktemp -d)"
cleanup() { rm -rf "\$TMP"; }
trap cleanup EXIT

echo "Downloading Hark…"
if curl -fsSL "\$URL_VERSIONED" -o "\$TMP/pkg.tar.gz"; then
  :
elif curl -fsSL "\$URL_LATEST" -o "\$TMP/pkg.tar.gz"; then
  :
else
  echo "error: could not download package for \${ARCH}" >&2
  echo "  tried: \$URL_VERSIONED" >&2
  echo "  tried: \$URL_LATEST" >&2
  echo "Set the correct GitHub repo or install from source: https://github.com/\${REPO}" >&2
  exit 1
fi

tar -xzf "\$TMP/pkg.tar.gz" -C "\$TMP"
DIR="\$(find "\$TMP" -maxdepth 1 -type d -name 'hark-*' | head -1)"
if [[ -z "\$DIR" || ! -x "\$DIR/install.sh" ]]; then
  echo "error: unexpected package layout" >&2
  exit 1
fi

bash "\$DIR/install.sh" "\${@:-}"
EOF
chmod +x "$ONLINE_INSTALLER"

# Optional .deb via cargo-deb
if command -v cargo-deb >/dev/null 2>&1; then
  echo "==> Building .deb with cargo-deb"
  # cargo-deb uses [package.metadata.deb]
  if cargo deb --release "${FEATURES[@]}" -o "$DIST" 2>"$DIST/cargo-deb.log"; then
    echo "    .deb written under dist/"
  else
    echo "    cargo-deb failed (see dist/cargo-deb.log) — tarball still ok"
  fi
else
  echo "==> cargo-deb not installed — skipping .deb (optional: cargo install cargo-deb)"
fi

# Checksums
echo "==> Writing SHA256SUMS"
(
  cd "$DIST"
  # shellcheck disable=SC2035
  sha256sum *.tar.gz install.sh *.deb 2>/dev/null > SHA256SUMS || \
    sha256sum *.tar.gz install.sh > SHA256SUMS
)

# Cleanup stage (keep artifacts only)
rm -rf "$DIST/stage"

echo
echo "Done. Shareable artifacts:"
ls -lh "$DIST" | sed 's/^/  /'
echo
echo "Share with friends:"
echo "  1) Upload dist/* to a GitHub Release (tag v${VERSION})"
echo "  2) They run:"
echo "       curl -fsSL https://github.com/${GITHUB_REPO}/releases/latest/download/install.sh | bash"
echo "     or:"
echo "       tar xzf ${PKG_NAME}.tar.gz && ./${PKG_NAME}/install.sh"
echo
echo "Local smoke-test without uploading:"
echo "  tar xzf $TARBALL -C /tmp && /tmp/${PKG_NAME}/install.sh"
